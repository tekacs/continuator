set positional-arguments := true
set dotenv-load := true

# List all available recipes.
default:
    @just --list

# Format the workspace.
fmt:
    cargo fmt

# Type-check without building binaries.
check:
    cargo check

# Run the CLI with arbitrary arguments, e.g. `just run create --id intro --prompt "..."`.
run *args:
    cargo run -- {{args}}

# Create a new clip with the CLI.
create id prompt:
    cargo run -- create --id {{id}} --prompt {{quote(prompt)}}

# Continue from an existing clip's last frame.
continue-clip parent id prompt:
    cargo run -- continue --from {{parent}} --id {{id}} --prompt {{quote(prompt)}}

# Download an asset variant for a clip.
download id variant output:
    cargo run -- download --id {{id}} --variant {{variant}} --output {{output}}
