# Building a Predictable Text Command

A text command should accept a file or standard input, write its primary result to standard output, and reserve standard error for diagnostics. The `--base-url` option supplies the context needed to preserve links when an HTML document is read outside its original page.

## Minimal Rust entry point

```rust
use std::io::{self, Read};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;

    if input.contains("<article") {
        println!("{}", input);
    }
    Ok(())
}
```

The fenced block must retain its language hint, four-space indentation, braces, ampersands, and angle brackets. Inline code remains separate from prose, while normal punctuation around it should not become part of the command token.

![Input flowing into a readable text document](https://example.test/guides/cli/images/input-to-text.png) A relative image used by this guide.

Continue with the [output contract](https://example.test/guides/reference/output-contract.html), review the [untrusted input policy](https://example.test/policies/untrusted-input), or jump to [verification](https://example.test/guides/cli/index.html#verification). These destinations should remain useful after the source page is gone.

## Verification

A fixture should confirm that code is unchanged, relative destinations are resolved against the supplied page URL, and fragment-only links stay attached to the current document rather than becoming unrelated paths.
