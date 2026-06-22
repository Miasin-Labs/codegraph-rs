use crate::extraction_test::fixture::*;

// describe('Objective-C Extraction')
// =============================================================================

const OBJC_SAMPLE: &str = "
#import <Foundation/Foundation.h>
#import \"MyClass.h\"

@interface MyClass : NSObject <NSCopying>
@property (nonatomic, copy) NSString *name;
- (void)greet;
- (void)doThing:(id)x with:(id)y;
+ (instancetype)shared;
@end

@implementation MyClass

- (void)greet {
    NSLog(@\"Hello\");
    [self doWork];
}

- (void)doThing:(id)x with:(id)y {
    [self notify:x];
}

+ (instancetype)shared {
    return [[MyClass alloc] init];
}

@end

void helperFunction(int count) {
    MyClass *obj = [MyClass shared];
    [obj greet];
}
";

#[test]
fn objc_extracts_classes_methods_functions_and_imports() {
    let result = extract("App.m", OBJC_SAMPLE);

    let classes = filter_kind(&result, NodeKind::Class);
    assert_eq!(classes.iter().filter(|c| c.name == "MyClass").count(), 1);

    let methods = filter_kind(&result, NodeKind::Method);
    let mut method_names = names(&methods);
    method_names.sort();
    assert_eq!(method_names, vec!["doThing:with:", "greet", "shared"]);

    let shared = methods.iter().find(|m| m.name == "shared").expect("shared");
    assert_eq!(shared.is_static, Some(true));

    let properties = filter_kind(&result, NodeKind::Property);
    assert!(properties.iter().any(|p| p.name == "name"));

    let functions = filter_kind(&result, NodeKind::Function);
    assert!(functions.iter().any(|f| f.name == "helperFunction"));

    let imports = names(&import_nodes(&result));
    assert!(imports.contains(&"Foundation/Foundation.h".to_string()));
    assert!(imports.contains(&"MyClass.h".to_string()));
}

#[test]
fn objc_records_inheritance_and_protocol_conformance() {
    let result = extract("App.m", OBJC_SAMPLE);
    let extends_refs = ref_names(&refs_of_kind(&result, EdgeKind::Extends));
    let implements_refs = ref_names(&refs_of_kind(&result, EdgeKind::Implements));
    assert!(extends_refs.contains(&"NSObject".to_string()));
    assert!(implements_refs.contains(&"NSCopying".to_string()));
}

#[test]
fn objc_records_message_sends_and_c_calls() {
    let result = extract("App.m", OBJC_SAMPLE);
    let calls = ref_names(&refs_of_kind(&result, EdgeKind::Calls));
    for expected in ["NSLog", "doWork", "MyClass.shared", "obj.greet"] {
        assert!(calls.contains(&expected.to_string()), "missing {expected}");
    }
}

#[test]
fn objc_reconstructs_multi_keyword_selectors_at_the_call_site() {
    // Regression for the gap discovered post-#165: message_expression's
    // multi-keyword form `[obj a:1 b:2]` was only emitting the first keyword,
    // so calls never resolved to multi-part method definitions like
    // `GET:parameters:headers:progress:success:failure:`. The call-site name
    // must match the method-definition name with full keywords + trailing colons.
    let code = "
@implementation Caller
- (void)demo {
    NSMutableDictionary *d = [NSMutableDictionary new];
    [d setObject:@\"v\" forKey:@\"k\"];
    [d setObject:@\"v2\" forKey:@\"k2\" withRetry:@YES];
    [self touchesBegan:nil withEvent:nil];
}
@end
";
    let result = extract("Caller.m", code);
    let calls = ref_names(&refs_of_kind(&result, EdgeKind::Calls));
    for expected in [
        "d.setObject:forKey:",
        "d.setObject:forKey:withRetry:",
        "touchesBegan:withEvent:",
    ] {
        assert!(calls.contains(&expected.to_string()), "missing {expected}");
    }
}

#[test]
fn objc_does_not_classify_pure_c_headers_with_at_end_in_comments_as_objc() {
    let c_header = "/* @end of file */\n#ifndef STDIO_H\nvoid printf(const char *);\n#endif\n";
    assert_eq!(detect_language("stdio.h", Some(c_header)), Language::C);
}

#[test]
fn objc_extracts_protocol_declarations() {
    let code = "
@protocol DataSource <NSObject>
- (NSInteger)numberOfItems;
@end
";
    let result = extract("DataSource.h", code);
    assert!(find_named(&result, NodeKind::Protocol, "DataSource").is_some());
}

#[test]
fn objc_is_reported_as_supported() {
    assert!(is_language_supported(Language::Objc));
    assert!(get_supported_languages().contains(&Language::Objc));
}
