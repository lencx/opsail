use std::ffi::OsString;
use std::mem::{MaybeUninit, size_of};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::windows::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::ptr;

use ::windows::ApplicationModel::{Package, PackageSignatureKind, PackageVersion};
use ::windows::Management::Deployment::PackageManager;
use ::windows::Win32::Foundation::{
    APPMODEL_ERROR_NO_APPLICATION, APPMODEL_ERROR_NO_PACKAGE, CloseHandle,
    ERROR_INSUFFICIENT_BUFFER, ERROR_NO_MORE_FILES, ERROR_SUCCESS, FILETIME, GetLastError, HANDLE,
    HLOCAL, LocalFree, WAIT_OBJECT_0,
};
use ::windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCP6TABLE_OWNER_PID, MIB_TCPROW_OWNER_PID,
    MIB_TCPTABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
};
use ::windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};
use ::windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use ::windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use ::windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdInfo, GetFileInformationByHandleEx, OPEN_EXISTING,
};
use ::windows::Win32::Storage::Packaging::Appx::{
    AppxFactory, GetApplicationUserModelId, GetPackageFamilyName, GetPackageFullName, IAppxFactory,
};
use ::windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, CoCreateInstance, CoTaskMemFree, STGM_READ,
    STGM_SHARE_DENY_WRITE,
};
use ::windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use ::windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_ACCESS_RIGHTS,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
};
use ::windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use ::windows::Win32::UI::Shell::{
    AO_NONE, ApplicationActivationManager, FOLDERID_LocalAppData, IApplicationActivationManager,
    KF_FLAG_DEFAULT, SHCreateStreamOnFileW, SHGetKnownFolderPath,
};
use ::windows::core::{HSTRING, PWSTR};

use super::{
    PrivatePathKind, ProcessIdentity, StoreAppCriteria, StoreAppIdentity, TcpListenerIdentity,
    WaitableProcess, WindowsPlatformError, windows_argument_line,
};

const MAX_PATH_UNITS: usize = 32_768;
const MAX_PACKAGE_APPLICATIONS: usize = 16;
const MAX_TCP_TABLE_BYTES: usize = 16 * 1024 * 1024;

type StorePackageVersion = (u16, u16, u16, u16);
type ValidatedStoreCandidate = (StorePackageVersion, StoreAppIdentity);

pub(crate) struct OwnedProcessHandle(HANDLE);

// SAFETY: a Windows kernel HANDLE may be used from another thread. This type
// owns one handle, never exposes the raw value, and closes it exactly once.
unsafe impl Send for OwnedProcessHandle {}

impl std::fmt::Debug for OwnedProcessHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("OwnedProcessHandle").finish()
    }
}

impl OwnedProcessHandle {
    fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> Result<Self, WindowsPlatformError> {
        // SAFETY: OpenProcess receives a concrete PID and no inherited handle. The
        // returned owned handle is closed exactly once by Drop.
        let handle = unsafe { OpenProcess(access, false, pid) }
            .map_err(|_| error("could not open the Windows process"))?;
        Ok(Self(handle))
    }

    pub(crate) fn wait(self) -> Result<(), WindowsPlatformError> {
        // SAFETY: self owns a valid process handle with SYNCHRONIZE access for the
        // duration of the blocking wait.
        let result = unsafe { WaitForSingleObject(self.0, u32::MAX) };
        if result == WAIT_OBJECT_0 {
            Ok(())
        } else {
            Err(error("could not wait for the Windows process"))
        }
    }
}

impl Drop for OwnedProcessHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by an owning Win32 API and is closed
        // exactly once here. Close failure cannot be recovered during Drop.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct OwnedFileHandle(HANDLE);

impl Drop for OwnedFileHandle {
    fn drop(&mut self) {
        // SAFETY: this handle came from CreateFileW and is closed exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

struct WinRtApartment;

impl WinRtApartment {
    fn initialize() -> Result<Self, WindowsPlatformError> {
        // SAFETY: every caller creates this guard before using WinRT or COM on
        // its thread. Each successful call is balanced by RoUninitialize.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }
            .map(|()| Self)
            .map_err(|_| error("could not initialize the Windows Runtime"))
    }
}

impl Drop for WinRtApartment {
    fn drop(&mut self) {
        // SAFETY: this balances the successful RoInitialize call on this thread.
        unsafe { RoUninitialize() };
    }
}

pub(crate) fn local_app_data_dir() -> Result<PathBuf, WindowsPlatformError> {
    // SAFETY: SHGetKnownFolderPath initializes `path` with a CoTaskMem-allocated,
    // NUL-terminated string on success. It is copied before being freed below.
    let path = unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, None) }
        .map_err(|_| error("could not resolve the Windows Local AppData directory"))?;
    // SAFETY: `path` is the valid NUL-terminated allocation returned above.
    let value = PathBuf::from(OsString::from_wide(unsafe { path.as_wide() }));
    // SAFETY: this frees the exact allocation returned by SHGetKnownFolderPath.
    unsafe { CoTaskMemFree(Some(path.0.cast())) };
    Ok(value)
}

pub(crate) fn find_store_app(
    criteria: StoreAppCriteria<'_>,
) -> Result<Option<StoreAppIdentity>, WindowsPlatformError> {
    validate_criteria(criteria)?;
    let _apartment = WinRtApartment::initialize()?;
    let manager = PackageManager::new()
        .map_err(|_| error("could not initialize Windows package discovery"))?;
    let packages = manager
        .FindPackagesByUserSecurityIdPackageFamilyName(
            &HSTRING::new(),
            &HSTRING::from(criteria.package_family_name),
        )
        .map_err(|_| error("could not enumerate the registered ChatGPT Store package"))?;

    let expected_aumid = format!(
        "{}!{}",
        criteria.package_family_name, criteria.application_id
    );
    let mut candidates = Vec::new();
    let mut found_registered_package = false;
    let iterator = packages
        .First()
        .map_err(|_| error("could not begin ChatGPT package enumeration"))?;
    while iterator
        .HasCurrent()
        .map_err(|_| error("could not inspect ChatGPT package enumeration"))?
    {
        let package = iterator
            .Current()
            .map_err(|_| error("could not read the registered ChatGPT package"))?;
        found_registered_package = true;
        if let Some(candidate) = validate_store_candidate(&package, criteria, &expected_aumid)? {
            candidates.push(candidate);
        }
        iterator
            .MoveNext()
            .map_err(|_| error("could not continue ChatGPT package enumeration"))?;
    }
    candidates.sort_by_key(|(version, _)| *version);
    match candidates.pop() {
        Some((_, identity)) => Ok(Some(identity)),
        None if found_registered_package => Err(error(
            "the registered ChatGPT Store package failed identity validation",
        )),
        None => Ok(None),
    }
}

fn validate_store_candidate(
    package: &Package,
    criteria: StoreAppCriteria<'_>,
    expected_aumid: &str,
) -> Result<Option<ValidatedStoreCandidate>, WindowsPlatformError> {
    let id = package
        .Id()
        .map_err(|_| error("could not inspect the ChatGPT package identity"))?;
    if id
        .Name()
        .map_err(|_| error("could not inspect the ChatGPT package name"))?
        != criteria.package_name
        || id
            .FamilyName()
            .map_err(|_| error("could not inspect the ChatGPT package family"))?
            != criteria.package_family_name
        || package
            .SignatureKind()
            .map_err(|_| error("could not inspect the ChatGPT package signature"))?
            != PackageSignatureKind::Store
        || package
            .IsDevelopmentMode()
            .map_err(|_| error("could not inspect the ChatGPT package mode"))?
        || !package
            .Status()
            .and_then(|status| status.VerifyIsOK())
            .map_err(|_| error("could not inspect the ChatGPT package status"))?
    {
        return Ok(None);
    }

    let entries = package
        .GetAppListEntries()
        .map_err(|_| error("could not inspect the ChatGPT package applications"))?;
    let entry_count = entries
        .Size()
        .map_err(|_| error("could not inspect the ChatGPT application count"))?;
    if entry_count == 0 || entry_count as usize > MAX_PACKAGE_APPLICATIONS {
        return Ok(None);
    }
    let mut matching_entries = 0usize;
    for index in 0..entry_count {
        let aumid = entries
            .GetAt(index)
            .and_then(|entry| entry.AppUserModelId())
            .map_err(|_| error("could not inspect the ChatGPT application identity"))?;
        if aumid == expected_aumid {
            matching_entries += 1;
        }
    }
    if matching_entries != 1 {
        return Ok(None);
    }

    let package_root = PathBuf::from(
        package
            .InstalledLocation()
            .and_then(|folder| folder.Path())
            .map_err(|_| error("could not resolve the ChatGPT package directory"))?
            .to_os_string(),
    );
    let Some(executable_relative_path) =
        manifest_executable(&package_root, criteria.application_id, expected_aumid)?
    else {
        return Ok(None);
    };
    let executable = package_root.join(executable_relative_path);
    let metadata = match std::fs::symlink_metadata(&executable) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(None),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Ok(None);
    }
    let canonical_root = match std::fs::canonicalize(&package_root) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    let canonical_executable = match std::fs::canonicalize(&executable) {
        Ok(path) if path.starts_with(&canonical_root) => path,
        _ => return Ok(None),
    };
    Ok(Some((
        package_version_tuple(
            id.Version()
                .map_err(|_| error("could not inspect the ChatGPT package version"))?,
        ),
        StoreAppIdentity {
            package_root: canonical_root,
            executable: canonical_executable,
            package_full_name: id
                .FullName()
                .map_err(|_| error("could not inspect the ChatGPT package full name"))?
                .to_string(),
            package_family_name: criteria.package_family_name.to_owned(),
            app_user_model_id: expected_aumid.to_owned(),
            user_sid: current_user_sid()?,
        },
    )))
}

fn manifest_executable(
    package_root: &Path,
    expected_application_id: &str,
    expected_aumid: &str,
) -> Result<Option<PathBuf>, WindowsPlatformError> {
    let manifest_path = HSTRING::from(package_root.join("AppxManifest.xml").as_os_str());
    // SAFETY: the path remains alive through the call. The returned COM stream
    // owns its file handle and is released before this function returns.
    let stream =
        unsafe { SHCreateStreamOnFileW(&manifest_path, STGM_READ.0 | STGM_SHARE_DENY_WRITE.0) }
            .map_err(|_| error("could not open the registered ChatGPT package manifest"))?;
    // SAFETY: AppxFactory is a fixed in-process system COM class and the current
    // thread has a live Windows Runtime apartment guard.
    let factory: IAppxFactory =
        unsafe { CoCreateInstance(&AppxFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(|_| error("could not create the Windows package manifest reader"))?;
    // SAFETY: stream is a live read-only IStream for the installed manifest.
    let reader = unsafe { factory.CreateManifestReader(&stream) }
        .map_err(|_| error("could not read the registered ChatGPT package manifest"))?;
    // SAFETY: reader is a live validated manifest reader.
    let applications = unsafe { reader.GetApplications() }
        .map_err(|_| error("could not enumerate the ChatGPT manifest applications"))?;

    let mut application_count = 0usize;
    let mut matching_applications = 0usize;
    let mut executable = None;
    loop {
        // SAFETY: applications remains live for the complete enumeration.
        let has_current = unsafe { applications.GetHasCurrent() }
            .map_err(|_| error("could not inspect the ChatGPT manifest applications"))?;
        if !has_current.as_bool() {
            break;
        }
        application_count += 1;
        if application_count > MAX_PACKAGE_APPLICATIONS {
            return Err(error(
                "the ChatGPT package manifest has too many applications",
            ));
        }
        // SAFETY: GetHasCurrent confirmed a current manifest application.
        let application = unsafe { applications.GetCurrent() }
            .map_err(|_| error("could not read a ChatGPT manifest application"))?;
        // SAFETY: the fixed attribute name is NUL-free and the returned string
        // is copied and freed by take_com_string.
        let application_id = take_com_string(
            unsafe { application.GetStringValue(&HSTRING::from("Id")) }
                .map_err(|_| error("could not read the ChatGPT manifest application ID"))?,
        )?;
        // SAFETY: the manifest application remains live through this call.
        let app_user_model_id = take_com_string(
            unsafe { application.GetAppUserModelId() }
                .map_err(|_| error("could not read the ChatGPT manifest AUMID"))?,
        )?;
        if application_id == expected_application_id && app_user_model_id == expected_aumid {
            matching_applications += 1;
            // SAFETY: the fixed attribute name is NUL-free and the returned
            // string is copied and freed by take_com_os_string.
            let relative = PathBuf::from(take_com_os_string(
                unsafe { application.GetStringValue(&HSTRING::from("Executable")) }
                    .map_err(|_| error("could not read the ChatGPT manifest executable"))?,
            )?);
            if !relative.is_absolute()
                && relative
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
            {
                executable = Some(relative);
            }
        }
        // SAFETY: applications remains live and MoveNext advances exactly once
        // for the current element.
        let has_next = unsafe { applications.MoveNext() }
            .map_err(|_| error("could not continue ChatGPT manifest enumeration"))?;
        if !has_next.as_bool() {
            break;
        }
    }
    if matching_applications == 1 {
        Ok(executable)
    } else {
        Ok(None)
    }
}

fn take_com_os_string(value: PWSTR) -> Result<OsString, WindowsPlatformError> {
    if value.is_null() {
        return Err(error("the ChatGPT manifest contains a null string"));
    }
    // SAFETY: Appx manifest APIs return a NUL-terminated CoTaskMem string.
    let result = OsString::from_wide(unsafe { value.as_wide() });
    // SAFETY: value is the exact CoTaskMem allocation returned by the API.
    unsafe { CoTaskMemFree(Some(value.0.cast())) };
    Ok(result)
}

fn take_com_string(value: PWSTR) -> Result<String, WindowsPlatformError> {
    take_com_os_string(value)?
        .into_string()
        .map_err(|_| error("the ChatGPT manifest identity is not valid Unicode"))
}

pub(crate) fn activate_application(
    app_user_model_id: &str,
    arguments: &[String],
) -> Result<WaitableProcess, WindowsPlatformError> {
    if !valid_aumid(app_user_model_id) || arguments.iter().any(|value| value.contains('\0')) {
        return Err(error(
            "the Windows application activation request is invalid",
        ));
    }
    let _apartment = WinRtApartment::initialize()?;
    // SAFETY: the CLSID and requested interface are fixed system contracts. COM
    // is initialized above and no raw COM pointer escapes this function.
    let manager: IApplicationActivationManager =
        unsafe { CoCreateInstance(&ApplicationActivationManager, None, CLSCTX_LOCAL_SERVER) }
            .map_err(|_| error("could not create the Windows application activation manager"))?;
    let aumid = HSTRING::from(app_user_model_id);
    let argument_line = HSTRING::from(windows_argument_line(arguments));
    // SAFETY: both HSTRING values stay alive for the duration of the COM call,
    // and the activation manager writes the PID into its own return value.
    let pid = unsafe { manager.ActivateApplication(&aumid, &argument_line, AO_NONE) }
        .map_err(|_| error("Windows could not activate the ChatGPT application"))?;
    if pid <= 1 {
        return Err(error("Windows returned an invalid ChatGPT process ID"));
    }
    let inner =
        OwnedProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE)?;
    let identity = identity_from_handle(pid, &inner)?;
    Ok(WaitableProcess { inner, identity })
}

pub(crate) fn process_identity(pid: u32) -> Result<ProcessIdentity, WindowsPlatformError> {
    if pid <= 1 {
        return Err(error("the Windows process ID is invalid"));
    }
    let handle = OwnedProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    identity_from_handle(pid, &handle)
}

pub(crate) fn process_instance_id(pid: u32) -> Result<u64, WindowsPlatformError> {
    let handle = OwnedProcessHandle::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    process_creation_time(&handle)
}

pub(crate) fn package_processes(
    package_family_name: &str,
) -> Result<Vec<ProcessIdentity>, WindowsPlatformError> {
    if !valid_identity_component(package_family_name, 128) {
        return Err(error("the Windows package family is invalid"));
    }
    // SAFETY: the returned snapshot handle is owned and closed by the RAII wrapper.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map(OwnedProcessHandle)
        .map_err(|_| error("could not enumerate Windows processes"))?;
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut processes = Vec::new();
    // SAFETY: entry points to a correctly sized, initialized PROCESSENTRY32W and
    // snapshot remains alive for the full enumeration.
    if unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_err() {
        return Err(error("could not read the Windows process list"));
    }
    loop {
        if let Ok(identity) = process_identity(entry.th32ProcessID)
            && identity
                .package_family_name
                .as_deref()
                .is_some_and(|family| family.eq_ignore_ascii_case(package_family_name))
        {
            processes.push(identity);
        }
        // SAFETY: the same initialized entry and live snapshot are reused as
        // required by Process32NextW.
        if unsafe { Process32NextW(snapshot.0, &mut entry) }.is_err() {
            // SAFETY: Process32NextW failed immediately before this call, so the
            // thread-local last-error value describes that failure.
            if unsafe { GetLastError() } == ERROR_NO_MORE_FILES {
                break;
            }
            return Err(error("could not continue the Windows process list"));
        }
    }
    processes.sort_by_key(|identity| (identity.created_at, identity.pid));
    Ok(processes)
}

pub(crate) fn tcp_listeners(port: u16) -> Result<Vec<TcpListenerIdentity>, WindowsPlatformError> {
    let mut listeners = ipv4_tcp_listeners(port)?;
    listeners.extend(ipv6_tcp_listeners(port)?);
    listeners.sort_unstable();
    listeners.dedup();
    Ok(listeners)
}

pub(crate) fn terminate_process(expected: &ProcessIdentity) -> Result<(), WindowsPlatformError> {
    if expected.pid <= 1 || expected.pid == std::process::id() || expected.created_at == 0 {
        return Err(error("refusing to terminate an invalid Windows process"));
    }
    let handle = OwnedProcessHandle::open(
        expected.pid,
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
    )?;
    let current = identity_from_handle(expected.pid, &handle)?;
    if &current != expected {
        return Err(error(
            "the Windows process identity changed before termination",
        ));
    }
    // SAFETY: the owned handle has PROCESS_TERMINATE access and its complete
    // identity was revalidated immediately before this call.
    unsafe { TerminateProcess(handle.0, 1) }
        .map_err(|_| error("could not stop the Windows process"))?;
    Ok(())
}

pub(crate) fn protect_current_user_path(
    path: &Path,
    kind: PrivatePathKind,
) -> Result<(), WindowsPlatformError> {
    let user_sid = current_user_sid()?;
    let inheritance = match kind {
        PrivatePathKind::Directory => "OICI",
        PrivatePathKind::File => "",
    };
    let sddl = HSTRING::from(format!(
        "D:P(A;{inheritance};FA;;;SY)(A;{inheritance};FA;;;{user_sid})"
    ));
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: descriptor is a valid out pointer and receives a LocalAlloc
    // allocation containing the parsed self-relative security descriptor.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            &sddl,
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(|_| error("could not construct the private Windows ACL"))?;

    let result = (|| {
        let mut present = false.into();
        let mut defaulted = false.into();
        let mut dacl = ptr::null_mut();
        // SAFETY: descriptor remains live and all output pointers are valid for
        // the duration of this call.
        unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted) }
            .map_err(|_| error("could not inspect the private Windows ACL"))?;
        if !present.as_bool() || dacl.is_null() {
            return Err(error("the private Windows ACL has no DACL"));
        }
        let path = HSTRING::from(path.as_os_str());
        // SAFETY: path and descriptor-backed DACL remain live through the call.
        let status = unsafe {
            SetNamedSecurityInfoW(
                &path,
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl),
                None,
            )
        };
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(error("could not apply the private Windows ACL"))
        }
    })();
    // SAFETY: descriptor is the exact LocalAlloc allocation returned above.
    unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    result
}

pub(crate) fn same_file(left: &Path, right: &Path) -> Result<bool, WindowsPlatformError> {
    Ok(file_id(left)? == file_id(right)?)
}

fn file_id(path: &Path) -> Result<FILE_ID_INFO, WindowsPlatformError> {
    let path = HSTRING::from(path.as_os_str());
    // SAFETY: path remains alive through CreateFileW and the returned owning
    // handle is closed by OwnedFileHandle.
    let handle = unsafe {
        CreateFileW(
            &path,
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map(OwnedFileHandle)
    .map_err(|_| error("could not open a Windows file identity"))?;
    let mut identity = FILE_ID_INFO::default();
    // SAFETY: identity is a correctly sized writable FILE_ID_INFO and the file
    // handle remains live for the query.
    unsafe {
        GetFileInformationByHandleEx(
            handle.0,
            FileIdInfo,
            (&mut identity as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    }
    .map_err(|_| error("could not read a Windows file identity"))?;
    Ok(identity)
}

fn identity_from_handle(
    pid: u32,
    handle: &OwnedProcessHandle,
) -> Result<ProcessIdentity, WindowsPlatformError> {
    Ok(ProcessIdentity {
        pid,
        executable: process_executable(handle)?,
        user_sid: process_user_sid(handle)?,
        created_at: process_creation_time(handle)?,
        package_full_name: process_package_name(handle, ProcessPackageField::FullName)?,
        package_family_name: process_package_name(handle, ProcessPackageField::FamilyName)?,
        app_user_model_id: process_package_name(handle, ProcessPackageField::AppUserModelId)?,
    })
}

fn process_executable(handle: &OwnedProcessHandle) -> Result<PathBuf, WindowsPlatformError> {
    let mut buffer = vec![0u16; MAX_PATH_UNITS];
    let mut length = buffer.len() as u32;
    // SAFETY: buffer is writable for length UTF-16 units and the live process
    // handle has PROCESS_QUERY_LIMITED_INFORMATION access.
    unsafe {
        QueryFullProcessImageNameW(
            handle.0,
            Default::default(),
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    }
    .map_err(|_| error("could not resolve the Windows process executable"))?;
    if length == 0 || length as usize > buffer.len() {
        return Err(error("the Windows process executable is invalid"));
    }
    Ok(PathBuf::from(OsString::from_wide(
        &buffer[..length as usize],
    )))
}

fn process_creation_time(handle: &OwnedProcessHandle) -> Result<u64, WindowsPlatformError> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all output pointers are valid initialized FILETIME values and the
    // live handle has process query access.
    unsafe { GetProcessTimes(handle.0, &mut creation, &mut exit, &mut kernel, &mut user) }
        .map_err(|_| error("could not inspect the Windows process start time"))?;
    let value = u64::from(creation.dwLowDateTime) | (u64::from(creation.dwHighDateTime) << 32);
    if value == 0 {
        Err(error("the Windows process start time is invalid"))
    } else {
        Ok(value)
    }
}

fn current_user_sid() -> Result<String, WindowsPlatformError> {
    let handle = OwnedProcessHandle::open(std::process::id(), PROCESS_QUERY_LIMITED_INFORMATION)?;
    process_user_sid(&handle)
}

fn process_user_sid(handle: &OwnedProcessHandle) -> Result<String, WindowsPlatformError> {
    let mut token = HANDLE::default();
    // SAFETY: token is a valid output location and handle is a live process handle.
    unsafe { OpenProcessToken(handle.0, TOKEN_QUERY, &mut token) }
        .map_err(|_| error("could not inspect the Windows process owner"))?;
    let token = OwnedProcessHandle(token);

    let mut required = 0u32;
    // SAFETY: a null first buffer is the documented size query; required is a
    // valid output pointer.
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut required) };
    if required < size_of::<TOKEN_USER>() as u32 || required as usize > 64 * 1024 {
        return Err(error("the Windows process owner identity is invalid"));
    }
    let words = (required as usize).div_ceil(size_of::<usize>());
    let mut buffer = vec![0usize; words];
    // SAFETY: the usize buffer is suitably aligned and writable for required
    // bytes. The token remains open while TOKEN_USER and its SID are consumed.
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            required,
            &mut required,
        )
    }
    .map_err(|_| error("could not read the Windows process owner"))?;
    // SAFETY: GetTokenInformation(TokenUser) initialized a TOKEN_USER at the
    // beginning of the aligned buffer, valid until buffer is dropped.
    let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut string_sid = PWSTR::null();
    // SAFETY: token_user.User.Sid is owned by the live token information buffer;
    // string_sid is initialized with a LocalAlloc allocation on success.
    unsafe { ConvertSidToStringSidW(token_user.User.Sid, &mut string_sid) }
        .map_err(|_| error("could not format the Windows process owner"))?;
    // SAFETY: ConvertSidToStringSidW returned a valid NUL-terminated string.
    let value = unsafe { string_sid.to_string() }
        .map_err(|_| error("the Windows process owner identity is invalid"));
    // SAFETY: string_sid is the exact LocalAlloc allocation returned above.
    unsafe { LocalFree(Some(HLOCAL(string_sid.0.cast()))) };
    value
}

#[derive(Clone, Copy)]
enum ProcessPackageField {
    FullName,
    FamilyName,
    AppUserModelId,
}

fn process_package_name(
    handle: &OwnedProcessHandle,
    field: ProcessPackageField,
) -> Result<Option<String>, WindowsPlatformError> {
    let mut required = 0u32;
    // SAFETY: a null first buffer is the documented size query and required is a
    // valid output pointer.
    let first = unsafe {
        match field {
            ProcessPackageField::FullName => GetPackageFullName(handle.0, &mut required, None),
            ProcessPackageField::FamilyName => GetPackageFamilyName(handle.0, &mut required, None),
            ProcessPackageField::AppUserModelId => {
                GetApplicationUserModelId(handle.0, &mut required, None)
            }
        }
    };
    if first == APPMODEL_ERROR_NO_PACKAGE || first == APPMODEL_ERROR_NO_APPLICATION {
        return Ok(None);
    }
    if first != ERROR_INSUFFICIENT_BUFFER || required == 0 || required as usize > 32 * 1024 {
        return Err(error(
            "could not inspect the Windows process package identity",
        ));
    }
    let mut buffer = vec![0u16; required as usize];
    // SAFETY: the UTF-16 buffer is writable for required elements and remains
    // alive through the API call and conversion.
    let second = unsafe {
        match field {
            ProcessPackageField::FullName => {
                GetPackageFullName(handle.0, &mut required, Some(PWSTR(buffer.as_mut_ptr())))
            }
            ProcessPackageField::FamilyName => {
                GetPackageFamilyName(handle.0, &mut required, Some(PWSTR(buffer.as_mut_ptr())))
            }
            ProcessPackageField::AppUserModelId => {
                GetApplicationUserModelId(handle.0, &mut required, Some(PWSTR(buffer.as_mut_ptr())))
            }
        }
    };
    if second != ERROR_SUCCESS || required == 0 || required as usize > buffer.len() {
        return Err(error("could not read the Windows process package identity"));
    }
    let length = required as usize - 1;
    String::from_utf16(&buffer[..length])
        .map(Some)
        .map_err(|_| error("the Windows process package identity is invalid"))
}

fn ipv4_tcp_listeners(port: u16) -> Result<Vec<TcpListenerIdentity>, WindowsPlatformError> {
    let buffer = tcp_table_buffer(AF_INET.0.into())?;
    if buffer.len() < size_of::<u32>() {
        return Err(error("the Windows IPv4 listener table is invalid"));
    }
    // SAFETY: tcp_table_buffer returns an aligned allocation initialized by
    // GetExtendedTcpTable. Bounds are checked before the row slice is formed.
    unsafe {
        let table = buffer.as_ptr().cast::<MIB_TCPTABLE_OWNER_PID>();
        let count = (*table).dwNumEntries as usize;
        let offset = ptr::addr_of!((*table).table) as usize - table as usize;
        let byte_len = count
            .checked_mul(size_of::<MIB_TCPROW_OWNER_PID>())
            .and_then(|rows| offset.checked_add(rows))
            .filter(|required| *required <= buffer.len())
            .ok_or_else(|| error("the Windows IPv4 listener table is invalid"))?;
        let _ = byte_len;
        let rows: &[MIB_TCPROW_OWNER_PID] =
            std::slice::from_raw_parts(ptr::addr_of!((*table).table).cast(), count);
        Ok(rows
            .iter()
            .filter(|row| u16::from_be(row.dwLocalPort as u16) == port)
            .map(|row| TcpListenerIdentity {
                address: IpAddr::V4(Ipv4Addr::from(row.dwLocalAddr.to_ne_bytes())),
                port,
                pid: row.dwOwningPid,
            })
            .collect())
    }
}

fn ipv6_tcp_listeners(port: u16) -> Result<Vec<TcpListenerIdentity>, WindowsPlatformError> {
    let buffer = tcp_table_buffer(AF_INET6.0.into())?;
    if buffer.len() < size_of::<u32>() {
        return Err(error("the Windows IPv6 listener table is invalid"));
    }
    // SAFETY: tcp_table_buffer returns an aligned allocation initialized by
    // GetExtendedTcpTable. Bounds are checked before the row slice is formed.
    unsafe {
        let table = buffer.as_ptr().cast::<MIB_TCP6TABLE_OWNER_PID>();
        let count = (*table).dwNumEntries as usize;
        let offset = ptr::addr_of!((*table).table) as usize - table as usize;
        count
            .checked_mul(size_of::<MIB_TCP6ROW_OWNER_PID>())
            .and_then(|rows| offset.checked_add(rows))
            .filter(|required| *required <= buffer.len())
            .ok_or_else(|| error("the Windows IPv6 listener table is invalid"))?;
        let rows: &[MIB_TCP6ROW_OWNER_PID] =
            std::slice::from_raw_parts(ptr::addr_of!((*table).table).cast(), count);
        Ok(rows
            .iter()
            .filter(|row| u16::from_be(row.dwLocalPort as u16) == port)
            .map(|row| TcpListenerIdentity {
                address: IpAddr::V6(Ipv6Addr::from(row.ucLocalAddr)),
                port,
                pid: row.dwOwningPid,
            })
            .collect())
    }
}

fn tcp_table_buffer(address_family: u32) -> Result<AlignedBuffer, WindowsPlatformError> {
    let mut required = 0u32;
    for _ in 0..4 {
        let mut buffer = if required == 0 {
            None
        } else {
            Some(AlignedBuffer::new(required as usize))
        };
        let table = buffer.as_mut().map(|buffer| buffer.as_mut_ptr().cast());
        // SAFETY: table is either the documented null size query or points to an
        // aligned writable allocation of `required` bytes. `required` remains a
        // valid in/out pointer and is revalidated before the next allocation.
        let result = unsafe {
            GetExtendedTcpTable(
                table,
                &mut required,
                false,
                address_family,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if result == ERROR_SUCCESS.0
            && let Some(mut buffer) = buffer
            && required as usize <= buffer.len()
        {
            buffer.truncate(required as usize);
            return Ok(buffer);
        }
        if result != ERROR_INSUFFICIENT_BUFFER.0
            || required < size_of::<u32>() as u32
            || required as usize > MAX_TCP_TABLE_BYTES
        {
            return Err(error("could not read the Windows TCP listener table"));
        }
    }
    Err(error("the Windows TCP listener table did not stabilize"))
}

struct AlignedBuffer {
    words: Vec<MaybeUninit<usize>>,
    bytes: usize,
}

impl AlignedBuffer {
    fn new(bytes: usize) -> Self {
        Self {
            words: vec![MaybeUninit::uninit(); bytes.div_ceil(size_of::<usize>())],
            bytes,
        }
    }

    fn as_ptr(&self) -> *const u8 {
        self.words.as_ptr().cast()
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.words.as_mut_ptr().cast()
    }

    fn len(&self) -> usize {
        self.bytes
    }

    fn truncate(&mut self, bytes: usize) {
        self.bytes = bytes;
    }
}

fn validate_criteria(criteria: StoreAppCriteria<'_>) -> Result<(), WindowsPlatformError> {
    if !valid_identity_component(criteria.package_name, 128)
        || !valid_identity_component(criteria.package_family_name, 128)
        || !valid_identity_component(criteria.application_id, 64)
    {
        Err(error("the Windows Store application contract is invalid"))
    } else {
        Ok(())
    }
}

fn valid_aumid(value: &str) -> bool {
    let Some((family, app)) = value.split_once('!') else {
        return false;
    };
    !app.contains('!') && valid_identity_component(family, 128) && valid_identity_component(app, 64)
}

fn valid_identity_component(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn package_version_tuple(version: PackageVersion) -> StorePackageVersion {
    (
        version.Major,
        version.Minor,
        version.Build,
        version.Revision,
    )
}

fn error(operation: &'static str) -> WindowsPlatformError {
    WindowsPlatformError::new(operation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::windows::Win32::System::WinRT::RO_INIT_SINGLETHREADED;

    #[test]
    fn known_folder_lookup_does_not_require_an_mta_thread() {
        let path = std::thread::spawn(|| {
            // SAFETY: this fresh test thread has no apartment. The successful
            // initialization is balanced before the thread returns.
            unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }.unwrap();
            let result = local_app_data_dir();
            // SAFETY: this balances the successful RoInitialize above.
            unsafe { RoUninitialize() };
            result
        })
        .join()
        .unwrap()
        .unwrap();

        assert!(path.is_absolute());
    }
}
