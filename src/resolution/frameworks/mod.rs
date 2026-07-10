//! Framework Resolver Registry
//!
//! Manages framework-specific resolvers.
//! Ported from `src/resolution/frameworks/index.ts`.
//!
//! Deviation from TS (documented in `notes/resolution-stitch.md`): the TS
//! module held a single mutable module-level `FRAMEWORK_RESOLVERS` array of
//! shared singletons. Several Rust resolvers carry interior-mutability
//! caches (`RefCell`/`Mutex`) keyed by project root, and the framework port
//! notes require a FRESH instance per resolver lifetime — so the registry
//! here is a constructor function: every call to
//! [`get_all_framework_resolvers`] / [`detect_frameworks`] builds new
//! instances in the exact TS registration order. `registerFrameworkResolver`
//! (global mutation, unused anywhere in the TS codebase or tests) has no
//! Rust equivalent; custom resolvers can be appended to the `Vec` a caller
//! owns.

use crate::resolution::types::{FrameworkResolver, ResolutionContext};
use crate::types::Language;

pub mod astro;
pub mod cargo_workspace;
pub mod cics;
pub mod csharp;
pub mod drupal;
pub mod expo_modules;
pub mod express;
pub mod fabric;
pub mod go;
pub mod goframe;
pub mod java;
pub mod laravel;
pub mod nestjs;
pub mod play;
pub mod python;
pub mod react;
pub mod react_native;
pub mod ruby;
pub mod rust;
pub mod salesforce;
pub mod svelte;
pub mod swift;
pub mod swift_objc;
pub mod terraform;
pub mod vue;

// Re-export framework resolvers (mirrors the TS `export { fooResolver }`
// re-exports at the bottom of frameworks/index.ts).
pub use astro::AstroResolver;
pub use cics::CicsResolver;
pub use csharp::AspnetResolver;
pub use drupal::DrupalResolver;
pub use expo_modules::ExpoModulesResolver;
pub use express::ExpressResolver;
pub use fabric::FabricViewResolver;
pub use go::GoResolver;
pub use goframe::GoFrameResolver;
pub use java::SpringResolver;
pub use laravel::{FACADE_MAPPINGS, LaravelResolver};
pub use nestjs::NestjsResolver;
pub use play::PlayResolver;
pub use python::{DjangoResolver, FastapiResolver, FlaskResolver};
pub use react::ReactResolver;
pub use react_native::ReactNativeBridgeResolver;
pub use ruby::RailsResolver;
pub use rust::RustResolver;
pub use salesforce::SalesforceResolver;
pub use svelte::SvelteResolver;
pub use swift::{SwiftUIResolver, UIKitResolver, VaporResolver};
pub use swift_objc::SwiftObjcBridgeResolver;
pub use terraform::TerraformResolver;
pub use vue::VueResolver;

/// All registered framework resolvers, in the exact TS
/// `FRAMEWORK_RESOLVERS` registration order. Fresh instances per call —
/// see module docs.
fn build_framework_resolvers() -> Vec<Box<dyn FrameworkResolver>> {
    vec![
        // PHP
        Box::new(LaravelResolver),
        Box::new(DrupalResolver),
        // JavaScript/TypeScript
        Box::new(ExpressResolver),
        Box::new(NestjsResolver),
        Box::new(ReactResolver),
        Box::new(SvelteResolver),
        Box::new(VueResolver),
        Box::new(AstroResolver),
        // Python
        Box::new(DjangoResolver),
        Box::new(FlaskResolver),
        Box::new(FastapiResolver),
        // Ruby
        Box::new(RailsResolver),
        // Java
        Box::new(SpringResolver),
        Box::new(PlayResolver),
        // Go
        Box::new(GoResolver),
        Box::new(GoFrameResolver),
        // Rust
        Box::new(RustResolver::new()),
        // C#
        Box::new(AspnetResolver),
        // Swift
        Box::new(SwiftUIResolver),
        Box::new(UIKitResolver),
        Box::new(VaporResolver),
        // Swift ↔ Objective-C cross-language bridging (mixed iOS apps)
        Box::new(SwiftObjcBridgeResolver::new()),
        // React Native JS ↔ native bridge (legacy + TurboModules)
        Box::new(ReactNativeBridgeResolver::new()),
        // Expo Modules — Function/AsyncFunction/Property DSL on Swift/Kotlin
        Box::new(ExpoModulesResolver),
        // React Native Fabric / Codegen view components — TS spec → component nodes
        Box::new(FabricViewResolver),
        // Mainframe transaction and infrastructure-as-code resolvers
        Box::new(CicsResolver::new()),
        Box::new(TerraformResolver),
        // Salesforce LWC ↔ Apex bridge (`@salesforce/apex/Class.method` imports)
        Box::new(SalesforceResolver),
    ]
}

/// Get all framework resolvers.
pub fn get_all_framework_resolvers() -> Vec<Box<dyn FrameworkResolver>> {
    build_framework_resolvers()
}

/// Get a resolver by name.
pub fn get_framework_resolver(name: &str) -> Option<Box<dyn FrameworkResolver>> {
    build_framework_resolvers()
        .into_iter()
        .find(|r| r.name() == name)
}

/// Detect which frameworks are used in a project.
///
/// TS wrapped each `detect()` in try/catch → false; Rust resolvers signal
/// failure by returning `false` rather than throwing, so the call is direct.
pub fn detect_frameworks(context: &dyn ResolutionContext) -> Vec<Box<dyn FrameworkResolver>> {
    build_framework_resolvers()
        .into_iter()
        .filter(|resolver| resolver.detect(context))
        .collect()
}

/// Filter a list of detected frameworks down to ones that apply to a given
/// language. Frameworks without an explicit `languages` list are treated as
/// universal.
pub fn get_applicable_frameworks(
    detected: &[Box<dyn FrameworkResolver>],
    language: Language,
) -> Vec<&dyn FrameworkResolver> {
    detected
        .iter()
        .filter(|fw| match fw.languages() {
            None => true,
            Some(langs) => langs.contains(&language),
        })
        .map(|fw| fw.as_ref())
        .collect()
}
