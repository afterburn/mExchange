# mExchange

A modular, high-performance cryptocurrency exchange platform built with Rust.

> **Work in Progress**: This project is under active development. The current implementation works but the architecture is evolving toward the target design described below.

![mExchange Trading Interface](docs/screenshot.png)

## Current Status

The exchange is functional with the following components:

| Component | Status | Description |
|-----------|--------|-------------|
| **matching_engine** | Complete | High-performance order matching library |
| **matching_engine_service** | Complete | REST/UDP wrapper for matching engine |
| **gateway** | Complete | Client-facing WebSocket/REST API |
| **accounts** | Complete | User auth, balances, settlement |
| **frontend** | Complete | React trading interface |
| **trading_bot** | Complete | Automated market making strategies |
| **market_data** | Partial | OHLCV aggregation (Kafka-dependent) |

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
Frontend (React) --> Gateway (Axum) <--UDP--> Matching Engine Service
                         |                         |
                         v                         v
                    Accounts Service ---------> PostgreSQL
```

### Custom UDP Protocol

Inter-service communication between the gateway and matching engine uses a custom FlatBuffers-based UDP protocol (`udp_proto/`) for minimal latency. This avoids HTTP overhead for the critical order/event path while maintaining type safety.

### Component Responsibilities

- **matching_engine/** - Pure Rust library for order matching. Price-time priority with BTreeMap for price levels. 5.2M orders/sec throughput.

- **matching_engine_service/** - Wraps the library, exposes REST API, communicates with gateway via UDP for low-latency order/event transport.

- **gateway/** - Client-facing server (port 3000). WebSocket for real-time orderbook/trades, REST for order placement. Proxies to accounts service.

- **accounts/** - User management, authentication (OTP-based), balance tracking, trade settlement, OHLCV aggregation.

- **frontend/** - React + TypeScript + Tailwind. Real-time orderbook, candlestick charts, order entry.

- **trading_bot/** - Automated trading strategies (MarketMaker, Aggressive, Random) for liquidity generation.

### API Endpoints

Gateway (port 3000):
- `WS /ws` - Real-time market data
- `POST /api/order` - Place order
- `GET /api/ohlcv` - OHLCV candle data
- `POST /auth/*` - Authentication

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

# Frontend
cd frontend && npm install && npm run build
```

### Running Tests

```bash
cargo test --manifest-path matching_engine/Cargo.toml
cargo test --manifest-path accounts/Cargo.toml
```

## Target Architecture (WIP)

The intended architecture includes additional components not yet fully implemented:

- **risk_engine** - Pre-trade risk validation
- **settlement** - Dedicated post-trade settlement service
- **admin** - Administrative dashboard and controls
- **Kafka** - Event streaming between services

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
