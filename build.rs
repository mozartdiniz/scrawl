/// Compile icons into a GResource bundle embedded in the binary, so the app
/// runs straight from `cargo run` without an install step.
fn main() {
    glib_build_tools::compile_resources(&["data"], "data/resources.gresource.xml", "scrawl.gresource");
}
