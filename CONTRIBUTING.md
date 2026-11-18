# Contributing

Bug reports, ideas and pull requests are welcome. For anything larger than a fix, open an issue first so we can agree
on the approach before you spend time on it. A privacy or security problem goes through [SECURITY.md](SECURITY.md),
not a public issue.

## Build

Windows 11, Rust 1.98 or newer (`rustup update`), and Visual Studio 2022's C++ tools (the "Desktop development with
C++" workload). From Git Bash:

```bash
scripts/fetch-pdfium.sh          # once: PDFium, for reading PDFs
scripts/fetch-onnxruntime.sh     # once: ONNX Runtime and DirectML, for text in images
cargo build --release            # target/release: excavate.exe and the reader, helper, Bedrock service and setup
```

Both libraries are optional: without them the build still works, and PDFs or text in images aren't read.

## Test

```bash
cargo test --workspace --release
cargo clippy --workspace --all-targets -- -D warnings
```

CI runs the same on every push and pull request (`.github/workflows/ci.yml`); a clippy warning fails it. The few lints
this code's style allows are listed in `Cargo.toml` (`[workspace.lints.clippy]`).

## Running a copy that leaves yours alone

If you use Excavate yourself, run the copy you're working on in a sandbox, so it doesn't touch your settings, your
index or your own Everything:

| Variable | What it does |
|---|---|
| `EXCAVATE_HOME=<folder>` | All settings, the index, models and logs go under that folder instead of `%APPDATA%\Excavate` and `%LOCALAPPDATA%\Excavate`. It runs beside your own copy without either hearing the other, and never touches Start with Windows. |
| `EXCAVATE_PRETEND_EVERYTHING` | Excavate never closes or starts Everything; it logs what it would have done. `running` pretends Everything is running, any other value that it isn't. |
| `EXCAVATE_NO_ELEVATE` | Excavate never asks Windows for administrator rights, so it never sets up Excavate Bedrock; it logs what it would have done. |

For example, from Git Bash:

```bash
EXCAVATE_HOME="$TEMP/excavate-try" EXCAVATE_PRETEND_EVERYTHING=1 EXCAVATE_NO_ELEVATE=1 target/release/excavate.exe
```

`excavate-indexer.exe --quit` stops the reader when you're done (with the same `EXCAVATE_HOME`). Deleting the folder
removes everything that copy kept.

## Code style

- Match the code around you. Lines are up to about 120 characters; there's no `rustfmt` configuration, so don't
  reformat files you aren't otherwise changing.
- Comments say why, in plain sentences. A public item gets a doc comment saying what it is for.
- Every `unsafe` block has a `// SAFETY:` comment.
- Nothing personal in the code, tests or commits: no paths from your PC, no real file names from your disk, no
  credentials, even fake-looking ones that aren't clearly made up.
- A new dependency needs a reason: say it in the pull request.
- Tests use temporary folders and never depend on what's installed on the PC that runs them.

## Pull requests

Keep a pull request to one change. Say what it changes and how you checked it, and for anything visible, add a
screenshot. CI has to pass.

By contributing, you agree that your contribution is licensed under the [MIT License](LICENSE).
