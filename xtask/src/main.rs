use std::env;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: cargo xtask <COMMAND>");
        eprintln!();
        eprintln!("Commands:");
        eprintln!("  codegen    Generate protobuf bindings from .proto files");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "codegen" => codegen()?,
        cmd => {
            eprintln!("Unknown command: {}", cmd);
            eprintln!();
            eprintln!("Available commands:");
            eprintln!("  codegen    Generate protobuf bindings from .proto files");
            std::process::exit(1);
        }
    }

    Ok(())
}

fn codegen() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")?;
    let workspace_root = PathBuf::from(manifest_dir).parent().unwrap().to_path_buf();
    let android_emulator_dir = workspace_root.join("android-emulator");
    let proto_path = android_emulator_dir.join("proto");
    let out_dir = android_emulator_dir.join("src").join("generated");

    // Ensure the output directory exists
    std::fs::create_dir_all(&out_dir)?;

    println!("Generating protobuf bindings...");
    println!("  Proto path: {}", proto_path.display());
    println!("  Output dir: {}", out_dir.display());

    tonic_prost_build::configure()
        .build_server(false)
        .build_client(true)
        .out_dir(&out_dir)
        .compile_protos(
            &[proto_path.join("emulator_controller.proto")],
            &[proto_path],
        )?;

    println!("Successfully generated android-emulator/src/generated/emulator_control_proto.rs");
    println!("\nBindings are ready to use!");

    Ok(())
}
