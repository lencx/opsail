# Two Compiler Examples

### A source example

First, inspect the source function:

```cpp
auto process_values(const std::vector<uint8_t> &values)
{
    return std::count_if(
        values.begin(),
        values.end(),
        [](uint8_t value) { return value % 2 == 0; }
    );
}
```

Next, compare the generated assembly:

```nasm
process_values:
    vpbroadcastb xmm1, byte ptr [rip + .mask]
    vmovd xmm2, dword ptr [rsi + rax]
    vpandn xmm2, xmm2, xmm1
    add rax, 4
    cmp r8, rax
    jne .loop
```

The two listings describe the same operation at different levels.
