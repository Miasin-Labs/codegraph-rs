//! Terraform/OpenTofu module-scope resolver.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::resolution::types::{
    FrameworkResolver,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{Language, Node, NodeKind};

static SCOPED_REF_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^module\.([^.\s:]+):(file$|var\.|output\.|remote-output\.)")
        .expect("valid Terraform scoped-reference regex")
});
static SCOPED_REF_PARTS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^module\.([^.\s:]+):(.+)$").expect("valid Terraform scoped-reference parser")
});
static REMOTE_STATE_SOURCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"/remote-state(?:/|$)").expect("valid Terraform remote-state regex")
});

#[derive(Debug, Default)]
pub struct TerraformResolver;

impl FrameworkResolver for TerraformResolver {
    fn name(&self) -> &str {
        "terraform"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Terraform])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        context.get_all_files().iter().any(|file| {
            file.ends_with(".tf") || file.ends_with(".tfvars") || file.ends_with(".tofu")
        })
    }

    fn claims_reference(&self, name: &str) -> bool {
        SCOPED_REF_RE.is_match(name)
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        if reference.language != Language::Terraform {
            return None;
        }

        let qualified_name = &reference.reference_name;
        let reference_dir = dir_of(&reference.file_path);

        if let Some(parts) = SCOPED_REF_PARTS_RE.captures(qualified_name) {
            let module_name = parts.get(1).expect("module name").as_str();
            let child = parts.get(2).expect("scoped child").as_str();
            return resolve_scoped_module_ref(
                reference,
                module_name,
                child,
                &reference_dir,
                context,
            );
        }

        let candidates = context.get_nodes_by_qualified_name(qualified_name);
        let same_directory = candidates
            .iter()
            .find(|candidate| dir_of(&candidate.file_path) == reference_dir);
        if let Some(target) = same_directory {
            return Some(framework_resolution(reference, &target.id, 0.95));
        }

        if reference.file_path.ends_with(".tfvars") && qualified_name.starts_with("var.") {
            if let Some(target) = nearest_ancestor_match(&candidates, &reference_dir) {
                return Some(framework_resolution(reference, &target.id, 0.9));
            }
        }

        if qualified_name.starts_with("provider.") {
            let provider_configs: Vec<Node> = candidates
                .into_iter()
                .filter(|candidate| candidate.kind == NodeKind::Namespace)
                .collect();
            if let Some(target) = nearest_ancestor_match(&provider_configs, &reference_dir) {
                return Some(framework_resolution(reference, &target.id, 0.9));
            }
        }

        None
    }
}

fn framework_resolution(reference: &UnresolvedRef, target: &str, confidence: f64) -> ResolvedRef {
    ResolvedRef {
        original: reference.clone(),
        target_node_id: target.to_string(),
        confidence,
        resolved_by: ResolvedBy::Framework,
    }
}

fn nearest_ancestor_match(candidates: &[Node], reference_dir: &str) -> Option<Node> {
    let mut directory = parent_of(reference_dir);
    while let Some(current) = directory {
        if let Some(candidate) = candidates
            .iter()
            .find(|candidate| dir_of(&candidate.file_path) == current)
        {
            return Some(candidate.clone());
        }
        directory = parent_of(&current);
    }
    None
}

fn resolve_scoped_module_ref(
    reference: &UnresolvedRef,
    module_name: &str,
    child: &str,
    reference_dir: &str,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let declarations: Vec<Node> = context
        .get_nodes_by_qualified_name(&format!("module.{module_name}"))
        .into_iter()
        .filter(|node| node.kind == NodeKind::Module)
        .collect();
    let declaration = declarations
        .iter()
        .find(|node| dir_of(&node.file_path) == reference_dir)
        .or_else(|| (declarations.len() == 1).then(|| &declarations[0]))?;
    let source = read_module_attr(declaration, "source", context)?;

    if let Some(output_name) = child.strip_prefix("remote-output.") {
        if !REMOTE_STATE_SOURCE_RE.is_match(&source) {
            return None;
        }
        let component = read_remote_component(declaration, context)?;
        let outputs: Vec<Node> = context
            .get_nodes_by_qualified_name(&format!("output.{output_name}"))
            .into_iter()
            .filter(|output| {
                let directory = dir_of(&output.file_path);
                directory == component || directory.ends_with(&format!("/{component}"))
            })
            .collect();
        let output_directories: HashSet<String> = outputs
            .iter()
            .map(|output| dir_of(&output.file_path))
            .collect();
        if outputs.is_empty() || output_directories.len() > 1 {
            return None;
        }
        return Some(framework_resolution(reference, &outputs[0].id, 0.9));
    }

    if !(source.starts_with("./") || source.starts_with("../")) {
        return None;
    }
    let target_directory = normalize_rel(&join_dirs(&dir_of(&declaration.file_path), &source));

    if child == "file" {
        let mut terraform_files: Vec<String> = context
            .get_all_files()
            .into_iter()
            .filter(|file| {
                dir_of(file) == target_directory
                    && (file.ends_with(".tf") || file.ends_with(".tofu"))
            })
            .collect();
        terraform_files.sort();
        let entry = terraform_files
            .iter()
            .find(|file| file.ends_with("/main.tf") || file.as_str() == "main.tf")
            .or_else(|| terraform_files.first())?;
        let file_node = context
            .get_nodes_in_file(entry)
            .into_iter()
            .find(|node| node.kind == NodeKind::File)?;
        return Some(framework_resolution(reference, &file_node.id, 0.95));
    }

    let target = context
        .get_nodes_by_qualified_name(child)
        .into_iter()
        .find(|candidate| dir_of(&candidate.file_path) == target_directory)?;
    Some(framework_resolution(reference, &target.id, 0.95))
}

fn read_remote_component(declaration: &Node, context: &dyn ResolutionContext) -> Option<String> {
    if let Some(component) = read_module_attr(declaration, "component", context) {
        return Some(component);
    }

    static COMPONENT_VARIABLE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*component\s*=\s*var\.([A-Za-z0-9_-]+)\s*$")
            .expect("valid Terraform component variable regex")
    });
    static DEFAULT_LITERAL_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"^\s*default\s*=\s*"([^"]+)""#).expect("valid Terraform default literal regex")
    });

    let variable = read_node_span_match(declaration, &COMPONENT_VARIABLE_RE, context)?;
    let declarations: Vec<Node> = context
        .get_nodes_by_qualified_name(&format!("var.{variable}"))
        .into_iter()
        .filter(|node| dir_of(&node.file_path) == dir_of(&declaration.file_path))
        .collect();
    if declarations.len() != 1 {
        return None;
    }
    read_node_span_match(&declarations[0], &DEFAULT_LITERAL_RE, context)
}

fn read_module_attr(
    declaration: &Node,
    name: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let pattern = Regex::new(&format!(r#"^\s*{}\s*=\s*"([^"]+)""#, regex::escape(name))).ok()?;
    read_node_span_match(declaration, &pattern, context)
}

fn read_node_span_match(
    node: &Node,
    pattern: &Regex,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let content = context.read_file(&node.file_path)?;
    let lines: Vec<&str> = content.split('\n').collect();
    let start = node.start_line.saturating_sub(1) as usize;
    let end = (node.end_line as usize).min(lines.len());
    lines
        .get(start..end)?
        .iter()
        .find_map(|line| pattern.captures(line))
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str().to_string())
}

fn dir_of(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(directory, _)| {
            if directory.is_empty() {
                ".".to_string()
            } else {
                directory.to_string()
            }
        })
        .unwrap_or_else(|| ".".to_string())
}

fn parent_of(directory: &str) -> Option<String> {
    if directory.is_empty() || directory == "." {
        return None;
    }
    directory
        .rsplit_once('/')
        .map(|(parent, _)| {
            if parent.is_empty() {
                ".".to_string()
            } else {
                parent.to_string()
            }
        })
        .or_else(|| Some(".".to_string()))
}

fn join_dirs(base: &str, relative: &str) -> String {
    if base == "." || base.is_empty() {
        relative.to_string()
    } else {
        format!("{base}/{relative}")
    }
}

fn normalize_rel(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let mut segments: Vec<&str> = Vec::new();
    for segment in normalized.split('/') {
        match segment {
            "" | "." => {}
            ".." if segments.last().is_some_and(|last| *last != "..") => {
                segments.pop();
            }
            ".." => segments.push(segment),
            _ => segments.push(segment),
        }
    }
    if segments.is_empty() {
        ".".to_string()
    } else {
        segments.join("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terraform_node(id: &str, path: &str) -> Node {
        Node::new(
            id,
            NodeKind::Variable,
            "project_id",
            "var.project_id",
            path,
            Language::Terraform,
            1,
            1,
        )
    }

    #[test]
    fn normalizes_local_module_sources() {
        assert_eq!(normalize_rel("modules/api/../vpc/./"), "modules/vpc");
        assert_eq!(normalize_rel("./"), ".");
        assert_eq!(
            join_dirs("modules/app", "../shared"),
            "modules/app/../shared"
        );
    }

    #[test]
    fn chooses_nearest_ancestor_for_var_files() {
        let candidates = vec![
            terraform_node("root", "variables.tf"),
            terraform_node("env", "envs/variables.tf"),
        ];
        let target =
            nearest_ancestor_match(&candidates, "envs/prod").expect("nearest ancestor declaration");
        assert_eq!(target.id, "env");
    }

    #[test]
    fn claims_only_module_boundary_references() {
        let resolver = TerraformResolver;
        assert!(resolver.claims_reference("module.vpc:file"));
        assert!(resolver.claims_reference("module.vpc:var.cidr"));
        assert!(resolver.claims_reference("module.state:remote-output.id"));
        assert!(!resolver.claims_reference("module.vpc.id"));
    }
}
