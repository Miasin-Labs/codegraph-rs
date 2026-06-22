use crate::extraction_test::fixture::*;

// describe('C# Extraction')
// =============================================================================

#[test]
fn csharp_extracts_class_declarations() {
    let code = r#"
public class OrderService
{
    private readonly IOrderRepository _repository;

    public OrderService(IOrderRepository repository)
    {
        _repository = repository;
    }

    public async Task<Order> GetOrderAsync(string id)
    {
        return await _repository.FindByIdAsync(id);
    }
}
"#;
    let result = extract("OrderService.cs", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "OrderService");
    assert_eq!(class_node.visibility, Some(Visibility::Public));
}

// =============================================================================
