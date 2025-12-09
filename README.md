# mExchange

A modular, high-performance cryptocurrency exchange platform built with Rust.

> **Work in Progress**: This project is under active development. The current implementation works but the architecture is evolving toward the target design described below.

**Live Demo**: [exchange.kevin.rs](https://exchange.kevin.rs)

![mExchange Trading Interface](docs/screenshot.png)

## Current Status

The exchange is functional with the following components:

| Component | Type | Status | Description |
|-----------|------|--------|-------------|
| **matching_engine** | Library | Complete | High-performance order matching |
| **udp_proto** | Library | Complete | FlatBuffers-based UDP protocol definitions |
| **matching_engine_service** | Service | Complete | REST/UDP wrapper for matching engine |
| **gateway** | Service | Complete | Client-facing WebSocket/REST API |
| **accounts** | Service | Complete | User auth, balances, settlement |
| **market_data** | Service | Complete | OHLCV aggregation from gateway events |
| **trading_bot** | Service | Complete | Automated market making strategies |
| **frontend** | App | Complete | React trading interface |

## Quick Start

### Local Development (Docker)

```bash
docker-compose up --build
```

Access the frontend at `http://localhost:5173`

### Production Deployment

See `.github/workflows/deploy.yml` for EC2 deployment via GitHub Actions.

## Architecture

```
                                    ┌─────────────────────────────────────────┐
                                    │            Matching Engine              │
                                    │              Service                    │
                                    │  ┌───────────────────────────────────┐  │
   ┌──────────────┐                 │  │  matching_engine (lib)            │  │
   │   Frontend   │   WebSocket     │  │  - BTreeMap price levels          │  │
   │    React     │◄───────────────►│  │  - HashMap O(1) order lookup      │  │
   │   Vite/TS    │                 │  │  - Price-time priority            │  │
   └──────────────┘                 │  └───────────────────────────────────┘  │
          │                         └─────────────────────────────────────────┘
          │ HTTP                                   ▲
          ▼                                        │ UDP + FlatBuffers
   ┌──────────────┐                                │ (orders, fills, events)
   │   Gateway    │◄───────────────────────────────┘
   │    Axum      │
   │  Port 3000   │────────────────┬───────────────┐
   └──────────────┘                │               │
          │                        │               │ WebSocket (trades)
          │ HTTP (auth, balances)  │               ▼
          ▼                        │        ┌──────────────┐
   ┌──────────────┐                │        │  Market Data │
   │   Accounts   │◄───────────────┼───────►│   Service    │
   │   Service    │  HTTP          │        │  Port 3002   │
   │  Port 3001   │  (settlement)  │        └──────────────┘
   └──────────────┘                │               │
          │                        │               │
          ▼                        ▼               ▼
   ┌────────────────────────────────────────────────────┐
   │                    PostgreSQL                       │
   │   (users, balances, orders, trades, ohlcv)         │
   └────────────────────────────────────────────────────┘
```

### Why UDP + FlatBuffers?

The critical order path (order submission → matching → fill notification) uses UDP with FlatBuffers serialization instead of HTTP/JSON for several reasons:

**UDP over TCP/HTTP:**
- No connection handshake overhead (3-way TCP handshake adds ~1.5 RTT)
- No head-of-line blocking - each datagram is independent
- No Nagle's algorithm delays (though TCP_NODELAY helps)
- Smaller packet overhead (8-byte UDP header vs 20+ byte TCP header)
- For co-located services on reliable networks, TCP's reliability guarantees add unnecessary latency

**FlatBuffers over JSON/Protobuf:**
- Zero-copy deserialization - access fields directly from the buffer without parsing
- No memory allocation during read - critical for avoiding GC pauses
- Strongly typed with code generation - catches schema mismatches at compile time
- ~10-100x faster than JSON parsing, ~2-10x faster than Protobuf
- Fixed schema evolution rules prevent accidental breaking changes

**Trade-offs:**
- UDP is unreliable - we accept this for the hot path since the gateway can retry/reconcile
- FlatBuffers has a learning curve and requires schema compilation
- Debugging is harder than plaintext JSON

For non-latency-critical paths (auth, settlement, balance queries), we use standard HTTP/JSON.

### Current Limitations

The current architecture has several limitations that would need to be addressed for production scale:

1. **Single matching engine instance** - The matching engine runs as a single process. While it handles 5M+ orders/sec, horizontal scaling would require order routing/sharding by symbol.

2. **No event replay** - UDP is fire-and-forget. A persistent event log would enable replay for recovery and debugging.

3. **Coupled settlement** - Settlement happens synchronously in the accounts service. A dedicated settlement service with its own transaction log would be more robust.

4. **Limited observability** - Basic logging only. Production would need distributed tracing (Jaeger), metrics (Prometheus), and alerting.

5. **No redundancy** - Single points of failure throughout. Would need leader election, hot standbys, and automated failover.

### Component Responsibilities

- **matching_engine/** - Pure Rust library for order matching. Price-time priority with BTreeMap for price levels. 5.2M orders/sec throughput.

- **matching_engine_service/** - Wraps the library, exposes REST API, communicates with gateway via UDP for low-latency order/event transport.

- **gateway/** - Client-facing server (port 3000). WebSocket for real-time orderbook/trades, REST for order placement. Proxies to accounts and market_data services.

- **accounts/** - User management, authentication (OTP-based), balance tracking, trade settlement.

- **market_data/** - OHLCV candlestick aggregation. Subscribes to gateway WebSocket for trade events, stores candles in PostgreSQL, exposes REST API for historical data.

- **frontend/** - React + TypeScript + Tailwind. Real-time orderbook, candlestick charts, order entry.

- **trading_bot/** - Automated trading strategies (MarketMaker, Aggressive, Random) for liquidity generation.

### API Endpoints

Gateway (port 3000):
- `WS /ws` - Real-time market data (orderbook, trades)
- `POST /api/order` - Place order
- `GET /api/ohlcv` - OHLCV candle data (proxied to market_data)
- `POST /auth/*` - Authentication (proxied to accounts)
- `GET /api/balances` - User balances (proxied to accounts)
- `GET /api/orders` - User orders (proxied to accounts)

## Development

### Prerequisites

- Rust 1.70+ (edition 2021)
- Node.js 20+
- PostgreSQL 16+
- Docker (optional)

### Building

```bash
# Rust services
cargo build --release --manifest-path gateway/Cargo.toml
cargo build --release --manifest-path matching_engine_service/Cargo.toml
cargo build --release --manifest-path accounts/Cargo.toml
cargo build --release --manifest-path market_data/Cargo.toml

# Frontend
cd frontend && npm install && npm run build
```

### Running Tests

```bash
cargo test --manifest-path matching_engine/Cargo.toml
cargo test --manifest-path accounts/Cargo.toml
```

## Roadmap

### Multiple Markets & Custom Pairs

Currently the exchange supports a single hardcoded trading pair (KCN/EUR). Planned improvements:

- **Dynamic market creation** - API to create new trading pairs with configurable tick size, lot size, and trading hours
- **Market registry service** - Central registry of available markets with metadata (fees, limits, status)
- **Per-market matching engines** - Each market runs in its own matching engine instance, enabling horizontal scaling
- **Symbol routing** - Gateway routes orders to the appropriate matching engine based on symbol
- **Cross-margin support** - Use collateral from one asset to trade multiple pairs

### Derivatives Support

The current spot-only implementation could be extended to support derivatives:

- **Perpetual futures** - No-expiry contracts with funding rate mechanism to anchor to spot price
- **Mark price oracle** - External price feeds to prevent manipulation and calculate unrealized PnL
- **Liquidation engine** - Monitor positions and trigger liquidations when margin falls below maintenance level
- **Insurance fund** - Socialize losses from liquidations that can't be filled at bankruptcy price
- **Position management** - Track open positions, leverage, margin requirements per user
- **Funding rate calculation** - Periodic payments between longs and shorts based on premium/discount to spot

This would require significant additions: a risk engine for pre-trade margin checks, position tracking in the accounts service, and oracle integration for mark prices.

### Target Architecture

The intended architecture includes additional components not yet fully implemented:

- **risk_engine** - Pre-trade risk validation, margin checks, position limits
- **settlement** - Dedicated post-trade settlement service with transaction log
- **admin** - Administrative dashboard and controls
- **Redis** - Caching layer for orderbook snapshots and session data

## Design Philosophy

- **Separation of Concerns** - Each component has a single responsibility
- **Performance First** - Critical paths optimized for minimal allocations
- **Rust** - Zero-cost abstractions, memory safety, fearless concurrency

## Contributing

Contributions are welcome! Please:
- Ensure all tests pass
- Add tests for new functionality
- Follow the existing code style
- Run benchmarks to check for performance regressions
- Update documentation as needed

## License

MIT
