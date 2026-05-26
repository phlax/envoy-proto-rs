use anyhow::{bail, Context, Result};
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;
use walkdir::WalkDir;

const ENVOY_REPO: &str = "https://github.com/envoyproxy/envoy.git";
const XDS_REPO: &str = "https://github.com/cncf/xds.git";
const PGV_REPO: &str = "https://github.com/bufbuild/protoc-gen-validate.git";
const GOOGLEAPIS_REPO: &str = "https://github.com/googleapis/googleapis.git";
const CEL_REPO: &str = "https://github.com/google/cel-spec.git";
const OPENTELEMETRY_PROTO_REPO: &str = "https://github.com/open-telemetry/opentelemetry-proto.git";
const PROMETHEUS_CLIENT_MODEL_REPO: &str = "https://github.com/prometheus/client_model.git";

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("regen") => {
            let root = workspace_root()?;
            let generated = root.join("crates/envoy-proto/src/generated");
            let lib_rs = root.join("crates/envoy-proto/src/lib.rs");
            regen(&root, &generated, &lib_rs)
        }
        Some("check-regen") => {
            let root = workspace_root()?;
            check_regen(&root)
        }
        _ => {
            eprintln!("Usage: cargo xtask <regen|check-regen>");
            std::process::exit(2);
        }
    }
}

fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .context("failed to find workspace root")
}

fn check_regen(root: &Path) -> Result<()> {
    let tmp = TempDir::new().context("failed to create temporary directory")?;
    let generated = tmp.path().join("generated");
    let lib_rs = tmp.path().join("lib.rs");
    regen(root, &generated, &lib_rs)?;

    let committed_generated = root.join("crates/envoy-proto/src/generated");
    compare_dirs(&committed_generated, &generated)
        .context("generated sources differ; run `cargo xtask regen` and commit the changes")?;

    let committed_lib = root.join("crates/envoy-proto/src/lib.rs");
    compare_files(&committed_lib, &lib_rs).context(
        "crates/envoy-proto/src/lib.rs differs; run `cargo xtask regen` and commit the changes",
    )?;

    Ok(())
}

fn regen(root: &Path, generated_dir: &Path, lib_rs: &Path) -> Result<()> {
    let envoy_version = fs::read_to_string(root.join("ENVOY_VERSION"))
        .context("failed to read ENVOY_VERSION")?
        .trim()
        .to_string();
    if envoy_version.is_empty() {
        bail!("ENVOY_VERSION is empty");
    }

    let cache_dir = root.join("target/envoy-src").join(&envoy_version);
    fs::create_dir_all(&cache_dir).context("failed to create envoy cache directory")?;

    let envoy_dir = cache_dir.join("envoy");
    ensure_repo_ref(ENVOY_REPO, &envoy_dir, &envoy_version)?;

    let deps = parse_repository_locations(&envoy_dir.join("api/bazel/repository_locations.bzl"))?;

    let deps_dir = cache_dir.join("deps");
    fs::create_dir_all(&deps_dir).context("failed to create dependency cache directory")?;

    let xds_dir = deps_dir.join("xds");
    ensure_repo_ref(
        XDS_REPO,
        &xds_dir,
        deps.get("cncf/xds").context("missing cncf/xds pin")?,
    )?;

    let pgv_dir = deps_dir.join("protoc-gen-validate");
    ensure_repo_ref(
        PGV_REPO,
        &pgv_dir,
        deps.get("bufbuild/protoc-gen-validate")
            .context("missing bufbuild/protoc-gen-validate pin")?,
    )?;

    let googleapis_dir = deps_dir.join("googleapis");
    ensure_repo_ref(
        GOOGLEAPIS_REPO,
        &googleapis_dir,
        deps.get("googleapis/googleapis")
            .context("missing googleapis/googleapis pin")?,
    )?;

    let cel_dir = deps_dir.join("cel-spec");
    ensure_repo_ref(
        CEL_REPO,
        &cel_dir,
        deps.get("google/cel-spec")
            .context("missing google/cel-spec pin")?,
    )?;

    let opentelemetry_proto_dir = deps_dir.join("opentelemetry-proto");
    ensure_repo_ref(
        OPENTELEMETRY_PROTO_REPO,
        &opentelemetry_proto_dir,
        deps.get("open-telemetry/opentelemetry-proto")
            .context("missing open-telemetry/opentelemetry-proto pin")?,
    )?;

    let prometheus_client_model_dir = deps_dir.join("prometheus-client-model");
    ensure_repo_ref(
        PROMETHEUS_CLIENT_MODEL_REPO,
        &prometheus_client_model_dir,
        deps.get("prometheus/client_model")
            .context("missing prometheus/client_model pin")?,
    )?;

    fs::create_dir_all(generated_dir).context("failed to create output directory")?;
    clear_directory(generated_dir).context("failed to clear output directory")?;

    let proto_roots = [
        envoy_dir.join("api/envoy"),
        envoy_dir.join("api/contrib"),
        xds_dir.clone(),
        pgv_dir.join("validate"),
    ];

    let protos = collect_protos(&proto_roots)?;
    if protos.is_empty() {
        bail!("no .proto files found for generation");
    }

    let includes = vec![
        envoy_dir.join("api"),
        xds_dir.clone(),
        pgv_dir.clone(),
        googleapis_dir,
        cel_dir.join("proto"),
        opentelemetry_proto_dir,
        prometheus_client_model_dir,
    ];

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .build_transport(false)
        .out_dir(generated_dir)
        .compile_protos(&protos, &includes)
        .context("failed to generate proto bindings")?;

    rustfmt_directory(generated_dir).context("failed to rustfmt generated sources")?;
    write_lib_rs(generated_dir, lib_rs).context("failed to write crates/envoy-proto/src/lib.rs")?;
    run_command(
        Command::new("rustfmt")
            .arg("--edition")
            .arg("2021")
            .arg(lib_rs),
        "rustfmt",
    )
    .context("failed to rustfmt crates/envoy-proto/src/lib.rs")?;

    Ok(())
}

fn ensure_repo_ref(repo: &str, dir: &Path, reference: &str) -> Result<()> {
    if !dir.exists() {
        run_command(
            Command::new("git")
                .arg("clone")
                .arg(repo)
                .arg(dir)
                .current_dir(dir.parent().context("missing parent directory")?),
            "git clone",
        )?;
    }

    run_command(
        Command::new("git")
            .arg("fetch")
            .arg("origin")
            .arg(reference)
            .arg("--tags")
            .arg("--force")
            .current_dir(dir),
        "git fetch",
    )?;

    run_command(
        Command::new("git")
            .arg("checkout")
            .arg("--force")
            .arg(reference)
            .current_dir(dir),
        "git checkout",
    )?;

    Ok(())
}

fn parse_repository_locations(path: &Path) -> Result<BTreeMap<String, String>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;

    let mut pins = BTreeMap::new();
    pins.insert("cncf/xds".to_string(), parse_dep_pin(&content, "xds")?);
    pins.insert(
        "bufbuild/protoc-gen-validate".to_string(),
        parse_dep_pin(&content, "com_envoyproxy_protoc_gen_validate")?,
    );
    pins.insert(
        "googleapis/googleapis".to_string(),
        parse_dep_pin(&content, "com_google_googleapis")?,
    );
    pins.insert(
        "google/cel-spec".to_string(),
        parse_dep_pin(&content, "dev_cel")?,
    );
    pins.insert(
        "open-telemetry/opentelemetry-proto".to_string(),
        parse_dep_pin(&content, "opentelemetry_proto")?,
    );
    pins.insert(
        "prometheus/client_model".to_string(),
        parse_dep_pin(&content, "prometheus_metrics_model")?,
    );

    Ok(pins)
}

fn parse_dep_pin(content: &str, key: &str) -> Result<String> {
    let start = content
        .find(&format!("{key} = dict("))
        .with_context(|| format!("failed to find key {key} in repository_locations.bzl"))?;
    let end = content[start..]
        .find("\n    ),")
        .map(|offset| start + offset)
        .unwrap_or(content.len());
    let block = &content[start..end];

    let version_re = Regex::new(r#"version\s*=\s*"([^"]+)""#)?;
    let version = version_re
        .captures(block)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str())
        .with_context(|| format!("failed to parse version for key {key}"))?;

    let url_re = Regex::new(r#"urls\s*=\s*\[\s*"([^"]+)""#)?;
    let url_template = url_re
        .captures(block)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str())
        .with_context(|| format!("failed to parse url template for key {key}"))?;

    let resolved_url = url_template.replace("{version}", version);
    let mut reference = resolved_url
        .rsplit_once("/archive/")
        .map(|(_, right)| right)
        .with_context(|| format!("unexpected archive URL for key {key}: {resolved_url}"))?;
    reference = reference
        .trim_end_matches(".tar.gz")
        .trim_end_matches(".zip");
    reference = reference
        .trim_start_matches("refs/tags/")
        .trim_start_matches("refs/heads/");

    if reference.is_empty() {
        bail!("failed to parse repository pin for key {key}");
    }

    Ok(reference.to_string())
}

fn collect_protos(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut protos = Vec::new();

    for root in roots {
        if !root.exists() {
            continue;
        }

        for entry in WalkDir::new(root)
            .into_iter()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            if entry.path().extension() != Some(OsStr::new("proto")) {
                continue;
            }

            let normalized = entry.path().to_string_lossy().replace('\\', "/");
            if normalized.contains("/v2/")
                || normalized.contains("/v2alpha/")
                || normalized.contains("/v2alpha1/")
            {
                continue;
            }

            protos.push(entry.path().to_path_buf());
        }
    }

    protos.sort();
    Ok(protos)
}

fn rustfmt_directory(dir: &Path) -> Result<()> {
    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry.file_type().is_file() && entry.path().extension() == Some(OsStr::new("rs"))
        })
    {
        run_command(
            Command::new("rustfmt")
                .arg("--edition")
                .arg("2021")
                .arg(entry.path()),
            "rustfmt",
        )?;
    }

    Ok(())
}

#[derive(Default)]
struct ModuleNode {
    file_name: Option<String>,
    children: BTreeMap<String, ModuleNode>,
}

fn write_lib_rs(generated_dir: &Path, lib_rs: &Path) -> Result<()> {
    let mut root = ModuleNode::default();

    for entry in WalkDir::new(generated_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry.file_type().is_file() && entry.path().extension() == Some(OsStr::new("rs"))
        })
    {
        let file_name = entry
            .path()
            .file_name()
            .and_then(OsStr::to_str)
            .context("failed to read generated file name")?
            .to_string();

        let package = entry
            .path()
            .file_stem()
            .and_then(OsStr::to_str)
            .context("failed to read generated package name")?;

        let mut node = &mut root;
        for segment in package.split('.') {
            node = node.children.entry(segment.to_string()).or_default();
        }
        node.file_name = Some(file_name);
    }

    let mut output = String::from("// @generated by cargo xtask regen\n#![allow(clippy::all)]\n\n");
    render_modules(&root, 0, &mut output);

    fs::write(lib_rs, output).with_context(|| format!("failed to write {}", lib_rs.display()))
}

fn render_modules(node: &ModuleNode, indent: usize, output: &mut String) {
    for (segment, child) in &node.children {
        let padding = " ".repeat(indent);
        output.push_str(&format!(
            "{padding}pub mod {} {{\n",
            rust_identifier(segment)
        ));

        if let Some(file_name) = &child.file_name {
            let include_padding = " ".repeat(indent + 4);
            output.push_str(&format!(
                "{include_padding}include!(\"generated/{file_name}\");\n"
            ));
            output.push('\n');
        }
        render_modules(child, indent + 4, output);
        output.push_str(&format!("{padding}}}\n"));
    }
}

fn rust_identifier(segment: &str) -> String {
    let keywords: BTreeSet<&str> = [
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe",
        "use", "where", "while", "async", "await", "dyn",
    ]
    .into_iter()
    .collect();

    if keywords.contains(segment) {
        format!("r#{segment}")
    } else {
        segment.to_string()
    }
}

fn clear_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(path).with_context(|| format!("failed to read {}", path.display()))? {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path.is_dir() {
            fs::remove_dir_all(&entry_path)
                .with_context(|| format!("failed to remove {}", entry_path.display()))?;
        } else {
            fs::remove_file(&entry_path)
                .with_context(|| format!("failed to remove {}", entry_path.display()))?;
        }
    }

    Ok(())
}

fn compare_dirs(expected: &Path, actual: &Path) -> Result<()> {
    let expected_files = collect_file_map(expected)?;
    let actual_files = collect_file_map(actual)?;

    if expected_files != actual_files {
        bail!("generated file list/content does not match");
    }

    Ok(())
}

fn collect_file_map(root: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    let mut files = BTreeMap::new();

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let relative = entry
            .path()
            .strip_prefix(root)
            .with_context(|| format!("failed to relativize {}", entry.path().display()))?
            .to_path_buf();
        let contents = fs::read(entry.path())
            .with_context(|| format!("failed to read {}", entry.path().display()))?;
        files.insert(relative, contents);
    }

    Ok(files)
}

fn compare_files(expected: &Path, actual: &Path) -> Result<()> {
    let left =
        fs::read(expected).with_context(|| format!("failed to read {}", expected.display()))?;
    let right = fs::read(actual).with_context(|| format!("failed to read {}", actual.display()))?;

    if left != right {
        bail!("file contents differ");
    }

    Ok(())
}

fn run_command(command: &mut Command, name: &str) -> Result<()> {
    let status = command
        .status()
        .with_context(|| format!("failed to execute {name}"))?;

    if !status.success() {
        bail!("{name} failed with status {status}");
    }

    Ok(())
}
