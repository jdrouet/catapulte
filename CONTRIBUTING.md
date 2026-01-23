# Contributing to catapulte

Thank you for your interest in contributing to catapulte! This document outlines contribution opportunities and guidelines for getting started.

## Getting Started

### Prerequisites

- Rust (latest stable)

### Development Setup

```bash
# Clone the repository
git clone https://github.com/jdrouet/catapulte.git
cd catapulte

# Build the project
cargo build

# Run tests
cargo test

# Run with development config
cargo run
```

### Code Quality

Before submitting a PR, ensure your code passes all checks:

```bash
cargo fmt        # Format code
cargo clippy     # Lint
cargo test       # Run tests
cargo audit      # Security audit
```

## Contribution Opportunities

Below are areas where contributions would be valuable.

- Testing Improvements
- Documentation Improvements

---

## Code Style Guidelines

- Follow standard Rust conventions (`rustfmt`)
- Use `anyhow::Result` for error handling
- Add tracing instrumentation to new functions
- Write tests for new functionality
- Keep functions small and focused
- Document public APIs

### Testing Conventions

- Test function names should follow the pattern `fn should_do_this_when_that()`
  ```rust
  #[test]
  fn should_return_empty_list_when_no_packages_found() { ... }

  #[test]
  fn should_fail_when_database_connection_lost() { ... }
  ```

### Error Handling

- Do not use single letters for error variables
- Use `err` or `error` instead of `e` for readability
  ```rust
  // Bad
  .map_err(|e| anyhow::anyhow!("Failed: {e}"))

  // Good
  .map_err(|err| anyhow::anyhow!("Failed: {err}"))
  ```

### Code Coverage

Generate code coverage reports with:
```bash
cargo llvm-cov
```

## Architecture Notes

catapulte follows a hexagonal (ports & adapters) architecture:

- **Domain Layer**: Core business logic, independent of external systems
- **Adapters**: Implementations for external systems (HTTP, Mrml, etc.)
- **Traits**: Interfaces that adapters implement

When adding new features:
1. Define any new traits in the domain layer
2. Implement adapters for external integrations
3. Keep the domain layer free of framework dependencies

## Submitting Changes

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-feature`)
3. Make your changes
4. Run all checks (`cargo fmt && cargo clippy && cargo test`)
5. Commit with a descriptive message
6. Push to your fork
7. Open a Pull Request

### Commit Message Format

Use conventional commits:
- `feat:` New features
- `fix:` Bug fixes
- `docs:` Documentation changes
- `test:` Test additions or fixes
- `refactor:` Code refactoring
- `chore:` Maintenance tasks

### Pull Request Title

Pull request titles **must** follow the conventional commits format:
- `feat: add health check endpoint`
- `fix: handle empty package list gracefully`
- `docs: update configuration reference`

This ensures consistent changelog generation and release notes.

## Questions?

Feel free to open an issue for discussion before starting work on larger features. This helps ensure your contribution aligns with the project direction and avoids duplicate effort.
