mod aliases;
mod body_traversal;
mod calls;
mod context;
mod declarations;
mod decorators;
mod extractor;
mod functions;
mod imports;
mod inheritance;
mod instantiations;
mod members;
mod methods;
mod object_literals;
mod pascal;
mod pascal_calls;
mod pascal_declarations;
mod rust_relationships;
#[cfg(test)]
mod tests;
mod traversal;
mod type_annotations;
mod type_declarations;
mod variables;

pub use extractor::TreeSitterExtractor;
