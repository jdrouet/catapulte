# Hexagonal Architecture Roadmap

This document outlines the plan to refactor Catapulte to follow a hexagonal (ports & adapters) architecture.

## Goals

- Clear separation between business logic and infrastructure
- Domain layer independent of frameworks (Axum, Lettre, etc.)
- Testable business logic without spinning up HTTP servers or SMTP
- Native async traits (Rust 2024 edition)

## Target Structure

```
catapulte/
├── Cargo.toml                      # Workspace root
│
├── crates/
│   ├── catapulte-domain/           # Core business logic (no external dependencies)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── prelude.rs          # Port traits: TemplateLoader, EmailSender, etc.
│   │       ├── model/
│   │       │   ├── mod.rs
│   │       │   ├── email.rs        # Email, Recipients, Attachment
│   │       │   └── template.rs     # Template, TemplateMetadata
│   │       ├── service/
│   │       │   ├── mod.rs
│   │       │   └── send_email.rs   # Core use case orchestration
│   │       └── error.rs            # Domain errors
│   │
│   ├── catapulte-adapter-smtp/     # SMTP adapter (Lettre)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── sender.rs           # impl EmailSender
│   │       └── config.rs           # SMTP configuration
│   │
│   ├── catapulte-adapter-template/ # Template loading & rendering (mrml, handlebars)
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── loader/
│   │       │   ├── mod.rs
│   │       │   ├── local.rs        # impl TemplateLoader (filesystem)
│   │       │   └── http.rs         # impl TemplateLoader (remote)
│   │       ├── renderer.rs         # impl TemplateRenderer (mrml + handlebars)
│   │       └── config.rs
│   │
│   └── catapulte-adapter-http/     # HTTP adapter (Axum)
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── server.rs           # Axum setup, routing
│           ├── error.rs            # Domain errors → HTTP responses
│           └── controller/
│               ├── mod.rs
│               ├── status.rs       # Health check
│               ├── metrics.rs      # Prometheus endpoint
│               └── templates/
│                   ├── mod.rs
│                   ├── json.rs     # POST /templates/{name}/json
│                   └── multipart.rs
│
├── src/
│   ├── main.rs                     # Entry point, CLI, dependency injection
│   └── config.rs                   # App configuration
│
├── template/                       # Example templates (unchanged)
└── tests/                          # Integration tests
```

### Crate Dependency Graph

```
                    catapulte (binary)
                           │
          ┌────────────────┼────────────────┐
          │                │                │
          ▼                ▼                ▼
   adapter-http     adapter-smtp    adapter-template
          │                │                │
          └────────────────┼────────────────┘
                           │
                           ▼
                   catapulte-domain
                    (no dependencies)
```

## Port Traits (`crates/catapulte-domain/src/prelude.rs`)

```rust
pub trait TemplateLoader: Send + Sync {
    async fn load(&self, name: &str) -> Result<Template, TemplateLoadError>;
}

pub trait TemplateRenderer: Send + Sync {
    async fn render(
        &self,
        template: &Template,
        params: serde_json::Value,
    ) -> Result<RenderedEmail, RenderError>;
}

pub trait EmailSender: Send + Sync {
    async fn send(&self, email: Email) -> Result<(), SendError>;
    async fn test_connection(&self) -> Result<(), SendError>;
}
```

## Migration Phases

### Phase 1: Rust 2024 Edition

Migrate to Rust 2024 edition to enable native async traits.

**Changes:**
- Update `edition = "2024"` in all `Cargo.toml` files
- Fix `if let` patterns in `src/controller/templates/json.rs` and `multipart.rs` (temporary drop order)
- Verify tests pass

### Phase 2: Create Domain Crate

Create `crates/catapulte-domain/` with core business logic.

**Steps:**
1. Create `crates/catapulte-domain/Cargo.toml`
   - Minimal dependencies: `serde`, `thiserror`
   - No framework dependencies (no Axum, no Lettre, no mrml)

2. Create domain models in `src/model/`
   - `Email`, `Recipients`, `Attachment` - extracted from `lib/prelude`
   - `Template`, `TemplateMetadata` - framework-agnostic representations

3. Create `src/error.rs` with domain errors
   - `TemplateLoadError` - template not found, invalid metadata, IO errors
   - `RenderError` - interpolation failure, parse error
   - `SendError` - delivery failures
   - No `StatusCode` or HTTP concerns

4. Define port traits in `src/prelude.rs`
   - `TemplateLoader`, `TemplateRenderer`, `EmailSender`
   - Native async traits (Rust 2024, no `async_trait` macro)

5. Create `src/service/send_email.rs`
   - `SendEmailService<L, R, S>` generic over port implementations
   - Owns dependencies directly (no `Arc` wrapping inside)
   - Orchestrates: load template → render → send
   - Pure business logic

### Phase 3: Create Adapter Crates

**`crates/catapulte-adapter-smtp/`:**
- Depends on: `catapulte-domain`, `lettre`, `tokio`
- `SmtpSender` struct wrapping `lettre::AsyncSmtpTransport`
- Implements `EmailSender` trait
- Handles connection pooling, TLS, timeouts
- Configuration structs for SMTP settings

**`crates/catapulte-adapter-template/`:**
- Depends on: `catapulte-domain`, `mrml`, `handlebars`, `reqwest`
- `LocalLoader` - implements `TemplateLoader` (filesystem)
- `HttpLoader` - implements `TemplateLoader` (remote)
- `MrmlRenderer` - implements `TemplateRenderer`
- Configuration structs for loader/renderer settings

**`crates/catapulte-adapter-http/`:**
- Depends on: `catapulte-domain`, `axum`, `tower-http`
- `server.rs` - Axum router setup
- `error.rs` - `impl IntoResponse` for domain errors
- `controller/` - thin HTTP handlers
- Does NOT depend on other adapters (receives domain service via state)

### Phase 4: Wire Dependencies in Main Crate

Update root `src/` to wire everything together.

**Service ownership pattern:**

The `SendEmailService` owns its dependencies directly. The service itself is wrapped
in a single `Arc` for sharing across request handlers. This is more efficient than
wrapping each dependency in `Arc`:

- Single atomic increment per request (vs 3 with `Arc` per dependency)
- Better cache locality (one pointer to chase)
- Smaller state size in handlers (8 bytes vs 24 bytes)

```rust
// Domain service owns dependencies directly
struct SendEmailService<L, R, S> {
    loader: L,      // owned
    renderer: R,    // owned
    sender: S,      // owned
}

// main.rs - single Arc around the whole service
let loader = LocalLoader::new(&config.template);
let renderer = MrmlRenderer::new(&config.renderer);
let sender = SmtpSender::new(&config.smtp).await?;

let email_service = Arc::new(SendEmailService::new(loader, renderer, sender));

// Pass to Axum as state
let app = Router::new()
    .route("/templates/:name/json", post(json_handler))
    .route("/templates/:name/multipart", post(multipart_handler))
    .with_state(email_service);
```

**`src/config.rs`:**
- Unified configuration struct
- Delegates to adapter-specific configs

### Phase 5: Remove Legacy Crates

Delete `lib/engine/` and `lib/prelude/` after migration.

**Steps:**
1. Ensure all functionality is migrated to new crates
2. Update workspace members in root `Cargo.toml`
3. Remove `lib/` directory

## File Mapping (Current → Target)

| Current | Target |
|---------|--------|
| `lib/prelude/src/lib.rs` | `crates/catapulte-domain/src/model/` |
| `lib/engine/src/lib.rs` | `crates/catapulte-adapter-template/` + `crates/catapulte-domain/src/service/` |
| `lib/engine/src/loader/` | `crates/catapulte-adapter-template/src/loader/` |
| `src/controller/` | `crates/catapulte-adapter-http/src/controller/` |
| `src/service/server.rs` | `crates/catapulte-adapter-http/src/server.rs` |
| `src/service/smtp.rs` | `crates/catapulte-adapter-smtp/src/sender.rs` |
| `src/error.rs` | Split: `crates/catapulte-domain/src/error.rs` + `crates/catapulte-adapter-http/src/error.rs` |

## Testing Strategy

- **Domain tests**: Unit tests for `SendEmailService` with mock implementations of ports
- **Adapter tests**: Integration tests for each adapter (testcontainers for SMTP)
- **HTTP tests**: Existing integration tests in `tests/` remain mostly unchanged

## Non-Goals

- Changing external behavior or API contracts
- Adding new features during refactoring
- Changing configuration format
