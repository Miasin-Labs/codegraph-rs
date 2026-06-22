use crate::extraction_test::fixture::*;

// describe('Vue Extraction')
// =============================================================================

#[test]
fn vue_detects_vue_files() {
    assert_eq!(detect_language("App.vue", None), Language::Vue);
    assert_eq!(
        detect_language("components/Button.vue", None),
        Language::Vue
    );
    assert!(is_language_supported(Language::Vue));
}

#[test]
fn vue_extracts_component_node_from_a_vue_sfc() {
    let code = "<template>
  <div>{{ message }}</div>
</template>

<script>
export default {
  data() {
    return { message: 'Hello' };
  }
}
</script>
";
    let result = extract("HelloWorld.vue", code);

    let component_node = find_kind(&result, NodeKind::Component).expect("component");
    assert_eq!(component_node.name, "HelloWorld");
    assert_eq!(component_node.language, Language::Vue);
    assert_eq!(component_node.is_exported, Some(true));
}

#[test]
fn vue_extracts_functions_from_script_block() {
    let code = "<template>
  <button @click=\"handleClick\">Click</button>
</template>

<script>
function handleClick() {
  console.log('clicked');
}

const count = 0;
</script>
";
    let result = extract("Button.vue", code);

    let component_node = find_kind(&result, NodeKind::Component).expect("component");
    assert_eq!(component_node.name, "Button");

    let func_node = find_named(&result, NodeKind::Function, "handleClick").expect("handleClick");
    assert_eq!(func_node.language, Language::Vue);
}

#[test]
fn vue_extracts_from_script_setup_lang_ts_block() {
    let code = "<template>
  <div>{{ count }}</div>
</template>

<script setup lang=\"ts\">
import { ref } from 'vue';

const count = ref(0);

function increment(): void {
  count.value++;
}
</script>
";
    let result = extract("Counter.vue", code);

    let component_node = find_kind(&result, NodeKind::Component).expect("component");
    assert_eq!(component_node.name, "Counter");

    let func_node = find_named(&result, NodeKind::Function, "increment").expect("increment");
    assert_eq!(func_node.language, Language::Vue);

    // All nodes should be marked as vue language
    for node in &result.nodes {
        assert_eq!(node.language, Language::Vue);
    }
}

#[test]
fn vue_extracts_calls_from_top_level_script_setup_initializers() {
    let code = "<template>
  <div>{{ token }}</div>
</template>

<script setup lang=\"ts\">
import { getTokenMp } from './api/upload';

const token = getTokenMp();
</script>
";
    let result = extract("Issue425Setup.vue", code);

    assert!(find_ref(&result, EdgeKind::Calls, "getTokenMp").is_some());
}

#[test]
fn vue_extracts_calls_from_vue_options_api_object_methods() {
    let code = "<template>
  <button @click=\"save\">Save</button>
</template>

<script>
import { getTokenMp } from './api/upload';

export default {
  methods: {
    save() {
      return getTokenMp();
    }
  },
  setup() {
    return getTokenMp();
  }
}
</script>
";
    let result = extract("Issue425Options.vue", code);

    let calls: Vec<_> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "getTokenMp")
        .collect();
    assert_eq!(calls.len(), 2);
}

#[test]
fn vue_extracts_component_usages_from_the_vue_template_issue_629() {
    let code = "<template>
  <div class=\"wrap\">
    <UserCard :user=\"u\" />
    <my-button>Click</my-button>
    <Transition><span>x</span></Transition>
  </div>
</template>

<script setup lang=\"ts\">
import UserCard from './UserCard.vue';
import MyButton from './MyButton.vue';
</script>
";
    let result = extract("Host.vue", code);
    let refs = ref_names(&refs_of_kind(&result, EdgeKind::References));

    assert!(refs.contains(&"UserCard".to_string())); // PascalCase tag
    assert!(refs.contains(&"MyButton".to_string())); // kebab <my-button> → MyButton
    assert!(!refs.contains(&"Transition".to_string())); // Vue built-in skipped
    assert!(!refs.contains(&"Div".to_string())); // native HTML element skipped
    assert!(!refs.contains(&"Span".to_string()));
}

#[test]
fn vue_extracts_from_both_script_and_script_setup_blocks() {
    let code = "<template>
  <div>{{ msg }}</div>
</template>

<script>
export default {
  name: 'DualScript'
}
</script>

<script setup>
const msg = 'hello';

function greet() {
  return msg;
}
</script>
";
    let result = extract("DualScript.vue", code);

    assert!(find_kind(&result, NodeKind::Component).is_some());
    assert!(find_named(&result, NodeKind::Function, "greet").is_some());
}

#[test]
fn vue_creates_component_node_for_template_only_vue_file() {
    let code = "<template>
  <div>Static content</div>
</template>
";
    let result = extract("Static.vue", code);

    let component_node = find_kind(&result, NodeKind::Component).expect("component");
    assert_eq!(component_node.name, "Static");
    assert_eq!(component_node.language, Language::Vue);

    // Only the component node should exist (no script nodes)
    assert_eq!(result.nodes.len(), 1);
}

#[test]
fn vue_creates_containment_edges_from_component_to_script_nodes() {
    let code = "<template>
  <div>{{ value }}</div>
</template>

<script setup lang=\"ts\">
const value = 42;
</script>
";
    let result = extract("Contained.vue", code);

    let component_node = find_kind(&result, NodeKind::Component).expect("component");

    // Should have containment edges from component to child nodes
    let contain_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.source == component_node.id && e.kind == EdgeKind::Contains)
        .collect();
    assert!(!contain_edges.is_empty());
}

// =============================================================================
