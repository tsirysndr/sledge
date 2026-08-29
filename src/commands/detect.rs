use crate::card::Connected;

pub fn run(c: &Connected) {
    println!("Card: {}", c.kind.label());
}
