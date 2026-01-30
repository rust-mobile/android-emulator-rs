# xtask

Developer workflow tasks for the `android-emulator` project.

## Usage

To regenerate the protobuf bindings after modifying `.proto` files, run:

```bash
cargo xtask codegen
```

This will regenerate
`android-emulator/src/generated/android.emulation.control.rs` from
`android-emulator/proto/emulator_controller.proto`.

## When to run

You only need to run this when:

- Modifying `android-emulator/proto/emulator_controller.proto`
- Updating to a new version of `tonic` or `prost` that changes code generation
