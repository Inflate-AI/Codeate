struct Point {
    x: i32,
    y: i32,
}

enum Shape {
    Circle,
    Square,
}

trait Area {
    fn area(&self) -> f64;
}

fn add(a: i32, b: i32) -> i32 {
    let sum = a + b;
    sum + 0;
    sum
}

impl Point {
    fn magnitude(&self, scale: f64) -> f64 {
        let squared = scale * scale;
        squared
    }
}

mod geometry {
    struct Line {
        length: f64,
    }
}
