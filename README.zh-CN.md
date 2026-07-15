# Opsail

[English](https://github.com/lencx/opsail/blob/main/README.md) | 简体中文

Opsail 是一个模块化 Rust CLI，为软件 Agent 提供小型、可组合的行动能力。首个行动 `read` 可将 HTTP(S) URL、本地文件或标准输入中的静态 HTML 转换为可阅读的 Markdown、清洗后的 HTML 或带版本号的 JSON。

Opsail 只提取接收到的 HTML；它不会执行 JavaScript、维护浏览器会话、登录网站、抓取链接或与页面交互。

## 安装

Opsail 需要 Rust 1.97 或更高版本。从 crates.io 安装最新版本：

```sh
cargo install opsail
```

验证安装：

```sh
opsail --version
```

## 读取 HTML

默认输出 Markdown：

```sh
opsail read https://example.com/article
opsail read ./article.html
opsail read - < article.html
```

可以选择其他输出形式、为非 URL 输入解析相对链接、投影单个字段，或将结果写入文件：

```sh
opsail read ./article.html --format html --output cleaned.html
opsail read - --base-url https://example.com/articles/ < article.html
opsail read ./article.html --format json
opsail read ./article.html --property title
```

`extract` 是 `read` 的可见别名。运行 `opsail read --help` 可查看请求头、超时、字节限制和输出选项。

### 输出契约

数据写入 stdout，或写入 `--output PATH` 指定的文件。诊断和提取警告写入 stderr，因此 stdout 可以安全地通过管道传递。每个成功结果都以换行符结尾；下游提前关闭管道会被视为正常结束。

| 退出码 | 含义 |
| --- | --- |
| `0` | 命令成功，或成功输出帮助/版本信息 |
| `1` | 获取、提取、序列化或写入失败 |
| `2` | 命令行用法无效 |

`--format json` 输出 schema 版本 `1`，包含以下顶层字段：

```text
schemaVersion
content
contentHtml
metadata
source
extraction
quality
warnings
```

`content` 是 Markdown，`contentHtml` 是清洗后的 HTML。元数据包含标题，以及可用时的作者、描述、站点、发布时间、图片、图标、语言、文字方向、规范 URL 和域名。`source`、`extraction` 和 `quality` 对象记录来源、提取过程和质量信号。

`--property` 接受：

```text
content, markdown, contentHtml, html, title, author, description, site,
published, modified, image, favicon, language, direction, url, canonicalUrl, domain,
wordCount, quality, source, extraction
```

使用 `--format json` 时，投影字段输出合法 JSON；使用 Markdown 或 HTML 格式时，标量字段输出纯文本，结构化字段输出格式化 JSON。

### 默认值与限制

- 最大输入为 5 MiB，可通过正数 `--max-bytes` 覆盖。
- 解析后的 DOM 最多包含 50,000 个元素，嵌套深度最多为 256 层。
- HTTP(S) 连接超时为 5 秒，总超时为 15 秒；`--timeout` 可覆盖总超时。
- 最多跟随 10 次重定向。
- URL 输入和 `--base-url` 必须使用 HTTP(S)，且不能包含用户名/密码凭据。
- 字符解码依次考虑 BOM、HTTP charset、HTML 元数据、UTF-8 有效性，最后回退到 Windows-1252。
- 获取的响应体必须表现为 HTML；如果声明了媒体类型，则必须是 HTML 或允许的通用文本/二进制类型。
- 文件输入必须是普通文件。除非提供 `--base-url`，文件中的链接会保持相对形式；URL 输入则以重定向后的最终 URL 解析链接。

字节和 DOM 限制可约束常见的资源耗尽路径，但它们不是安全沙箱。URL 获取能够访问宿主网络允许的目标，并遵循系统代理设置。提取出的文本和链接都应视为不可信数据；嵌入 Agent 时，应另行实施网络、文件系统和下游执行策略。

## 参与贡献

开发环境、模块边界、测试规则与验证命令见 [CONTRIBUTING.zh-CN.md](https://github.com/lencx/opsail/blob/main/CONTRIBUTING.zh-CN.md)。

## 许可证

Apache License 2.0
