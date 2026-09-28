pub fn area() -> f64 {
    1.0
}

pub fn bounded<T: crate::Shape>(shape: &T) -> f64 {
    shape.area() + crate::helper() as f64
}
