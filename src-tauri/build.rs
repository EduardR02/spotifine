fn main() {
    // Generate embedded files before rustc fingerprints its inputs. Creating
    // them inside the context proc macro makes the next build look dirty.
    let attributes = tauri_build::Attributes::new().codegen(tauri_build::CodegenContext::new());
    tauri_build::try_build(attributes).expect("generate Tauri application context");
}
