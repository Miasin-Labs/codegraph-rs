pub mod other;

pub trait Shape {
    fn area(&self) -> f64;
}
pub struct Sq(pub f64);
pub struct Circle {
    pub r: f64,
}
impl Shape for Sq {
    fn area(&self) -> f64 {
        self.0 * self.0
    }
}
impl Shape for Circle {
    fn area(&self) -> f64 {
        3.14 * self.r * self.r
    }
}

macro_rules! make_fn {
    ($name:ident) => {
        pub fn $name() -> u32 {
            helper()
        }
    };
}
make_fn!(generated_one);

pub fn helper() -> u32 {
    7
}

pub fn total(shapes: &[Box<dyn Shape>]) -> f64 {
    shapes.iter().map(|s| s.area()).sum()
}

pub fn uses_macro() -> u32 {
    let v = vec![helper()];
    println!("{}", helper());
    generated_one() + v.len() as u32
}

pub fn generic<T: Shape>(t: &T) -> f64 {
    t.area()
}

pub fn call_sq() -> f64 {
    Sq(2.0).area() + generic(&Circle { r: 1.0 }) + other::area()
}
