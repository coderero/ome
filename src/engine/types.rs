use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone)]
pub struct Order {
    pub id: Uuid,
    pub side: Side,
    pub price: f64,
    pub qty: u32,
    pub timestamp: i64,
}
