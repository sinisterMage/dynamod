## What does this PR do?

<!-- A brief description of the change. Link to an issue if there is one. -->

## Testing

<!-- How did you test this? Which tests pass? -->

- [ ] `make test` passes
- [ ] Relevant QEMU test passes (if applicable)
- [ ] New tests added (if applicable)

## Checklist

- [ ] No systemd source code was referenced (clean-room implementations only)
- [ ] Zig code: no heap allocations in PID 1 path
- [ ] Rust code: `cargo fmt --all` was run
