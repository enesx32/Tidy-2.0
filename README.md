# Tidy-2.0

## How to Install
You can either download a pre-compiled binary from Releases or build it yourself.

### Option 1: Download Release
Go to **Releases** $\rightarrow$ **Latest Release**, download the executable, and add it to your system's `PATH` environment variables to run it globally.

### Option 2: Build from Source
Ensure you have Rust and Cargo installed, then run:

```bash
git clone [https://github.com/enesx32/Tidy-2.0](https://github.com/enesx32/Tidy-2.0)
cd Tidy-2.0
cargo build --release
cargo install --path . --force
```

## How to use
To use this you can run `tidy` to start from scratch

to open a file you can run `tidy [file_name]` normally

or to force a specific mode you can run `tidy [file_name] --[edit / view]`

you can also look at the lua extention file with `tidy --extensions --view`
