# Order Matching Engine

A high-performance order matching engine built in Rust that processes buy and sell orders with real-time OHLC (Open, High, Low, Close) tracking.

## Features

- **Real-time Order Matching**: Processes buy and sell orders with price-time priority
- **OHLC Tracking**: Maintains Open, High, Low, Close price data with timestamps
- **Interactive CLI**: Command-line interface for entering orders
- **Efficient Data Structures**: Uses binary heaps and indexed maps for optimal performance
- **UUID Order Tracking**: Each order is uniquely identified with UUIDs
- **Comprehensive Order Book Display**: Shows current state of buy and sell orders

## Architecture

The project is structured into several key modules:

- **`engine/order_book.rs`**: Core order matching logic with [`OrderBook`](src/engine/order_book.rs) struct
- **`engine/types.rs`**: Data structures including [`Order`](src/engine/types.rs) and [`Side`](src/engine/types.rs) enums
- **`engine/ohlc.rs`**: OHLC price tracking with [`OhlcTracker`](src/engine/ohlc.rs)
- **`utils/input.rs`**: CLI input handling for order entry

## Technical Implementation

### Order Book Structure

The [`OrderBook`](src/engine/order_book.rs) uses:

- **IndexMap**: For price-level order queues with insertion order preservation
- **BinaryHeap**: For efficient price priority management
- **VecDeque**: For FIFO order processing within price levels

### Float Handling

The [`OrderedF64`](src/engine/order_book.rs) wrapper enables floating-point prices to be used as hash keys and in binary heaps by implementing `Ord`, `Hash`, and `Eq` traits.

### Matching Algorithm

1. **Price-Time Priority**: Orders are matched based on best price first, then by arrival time
2. **Partial Fills**: Orders can be partially filled across multiple matches
3. **Quantity Management**: Automatic quantity updates and order removal when fully filled

## Installation

### Prerequisites

- Rust 1.86.0 or later
- Cargo package manager

### Dependencies

```toml
[dependencies]
chrono = "0.4.41"      # Date/time handling
indexmap = "2.9.0"     # Ordered hash maps
uuid = "1.16.0"        # UUID generation
```

### Setup

1. Clone the repository:

```bash
git clone <repository-url>
cd order-matching-engine
```

2. Build the project:

```bash
cargo build
```

3. Run the application:

```bash
cargo run
```

## Usage

### Command Line Interface

The engine accepts orders through a simple CLI format:

```
>> buy 100.50 10
>> sell 101.00 5
>> buy 100.75 15
```

**Format**: `<side> <price> <quantity>`

- **Side**: `buy` or `sell`
- **Price**: Decimal number (e.g., `100.50`)
- **Quantity**: Integer number of shares/units

### Example Session

```
Enter orders (buy/sell price qty):
>> buy 100.00 10
Order Book:
SELL:
BUY:
100.00 qty:10 id:a1b2c3d4-e5f6-7890-abcd-ef1234567890
--------------------------

>> sell 99.50 5
Trade executed: 100.00 @ a1b2c3d4-e5f6-7890-abcd-ef1234567890 (qty: 5)
OHLC [2025-07-07T10:30:45.123Z] => O: 100.00, H: 100.00, L: 100.00, C: 100.00
Order Book:
SELL:
BUY:
100.00 qty:5 id:a1b2c3d4-e5f6-7890-abcd-ef1234567890
--------------------------
```

## Key Components

### Order Structure

```rust
pub struct Order {
    pub id: Uuid,           // Unique identifier
    pub side: Side,         // Buy or Sell
    pub price: f64,         // Order price
    pub qty: u32,           // Quantity
    pub timestamp: i64,     // Creation timestamp
}
```

### OHLC Tracking

The [`OhlcTracker`](src/engine/ohlc.rs) maintains:

- **Open**: First trade price
- **High**: Highest trade price
- **Low**: Lowest trade price
- **Close**: Most recent trade price
- **Timestamp**: Last trade timestamp

### Matching Logic

1. **Buy Orders**: Match against sell orders with price ≤ buy price
2. **Sell Orders**: Match against buy orders with price ≥ sell price
3. **Price Priority**: Best prices are matched first
4. **Time Priority**: Earlier orders at same price are matched first

## Performance Characteristics

- **Order Insertion**: O(log n) where n is number of price levels
- **Order Matching**: O(log n) for price lookup, O(1) for quantity matching
- **Memory Usage**: Efficient with minimal allocations during matching
- **Scalability**: Handles thousands of orders with sub-millisecond latency

## Development

### Running Tests

```bash
cargo test
```

### Building for Release

```bash
cargo build --release
```

### Code Structure

```
src/
├── main.rs              # Application entry point
├── engine/
│   ├── mod.rs          # Engine module declarations
│   ├── order_book.rs   # Core matching engine
│   ├── types.rs        # Data structures
│   └── ohlc.rs         # OHLC tracking
└── utils/
    ├── mod.rs          # Utility module declarations
    └── input.rs        # CLI input handling
```

## Future Enhancements

- WebSocket API for real-time order streaming
- Persistence layer for order history
- Market data export functionality
- Performance benchmarking suite
- Multi-symbol support
- Advanced order types (stop-loss, iceberg orders)

## License

This project is open source and available under the MIT License.

## Contributing

Contributions are welcome! Please feel free to submit pull requests or open issues for bugs and feature requests.
