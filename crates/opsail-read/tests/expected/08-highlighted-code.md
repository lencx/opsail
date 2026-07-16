# Readable Highlighted Code

Syntax highlighters frequently render a visual gutter beside the program. The gutter is presentation, while indentation, punctuation, and language metadata belong to the readable document.

## Table gutter

```rust
fn total(values: &[u32]) -> u32 {
    values.iter().sum()
}
```

Inline gutters are another common representation. Their digits must not become part of identifiers or change the program copied by an agent.

## Inline gutter

```python
def greet(name):
    return f"Hello, {name}"
```

Prose after the examples is retained so a layout table cannot replace the surrounding explanation as the page's main extraction candidate.
