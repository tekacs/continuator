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

# Create a new Sora clip via the CLI.
create-sora id prompt:
    cargo run -- create --id {{id}} --prompt {{quote(prompt)}}

# Continue from an existing Sora clip's last frame.
continue-sora parent id prompt:
    cargo run -- continue --from {{parent}} --id {{id}} --prompt {{quote(prompt)}}

# Create a new Veo clip via the CLI.
create-veo gcp_project gcp_location id prompt:
    cargo run -- --provider veo --gcp-project {{gcp_project}} --gcp-location {{gcp_location}} --model veo-3.0-generate-preview create --id {{id}} --prompt {{quote(prompt)}}

# Continue a Veo clip using the previous last frame.
continue-veo gcp_project gcp_location parent id prompt:
    cargo run -- --provider veo --gcp-project {{gcp_project}} --gcp-location {{gcp_location}} --model veo-3.0-generate-preview continue --from {{parent}} --id {{id}} --prompt {{quote(prompt)}}

# Download an asset variant for a clip.
download id variant output:
    cargo run -- download --id {{id}} --variant {{variant}} --output {{output}}

# Concatenate clips via the CLI.
stitch id *clips:
    cargo run -- stitch --id {{id}} {{clips}}
