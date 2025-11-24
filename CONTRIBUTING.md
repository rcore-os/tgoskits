# Contributing Guide

Thank you for your interest in the StarryOS project! We welcome contributions of all kinds.

## Before You Start

Before you start contributing, please make sure:

1. You have read and understood the project's goals and architecture
2. You are familiar with the Rust programming language
3. You have set up your development environment (see [README.md](./README.md))

## Development Workflow

### 1. Fork and Clone the Repository

```bash
# Fork the repository to your GitHub account, then clone
git clone https://github.com/YOUR_USERNAME/StarryOS.git
cd StarryOS
git submodule update --init --recursive
```

### 2. Create a Branch

```bash
# Create your feature branch from the latest main branch
git checkout main
git pull upstream main  # if you have upstream set up
git checkout -b feature/your-feature-name
```

### 3. Make Changes

- Write code
- Add tests (if applicable)
- Update documentation (if needed)

### 4. Code Quality Checks

Before committing, make sure:

#### Run Clippy

```bash
cargo clippy --all-targets --all-features -- -D warnings
```

**Important**: All code must pass clippy checks, which is also verified in CI.

#### Format Code

```bash
cargo fmt
```

**Note**: Although code formatting is not currently checked in CI, please make sure to format your code. The project uses a custom `rustfmt.toml` configuration.

### 5. Commit Changes

#### Commit Message Convention

We follow the [Conventional Commits](https://www.conventionalcommits.org/) specification. The commit message format is:

```
<type>(<scope>): <subject>

<body>

<footer>
```

**Types**:

- `feat`: A new feature
- `fix`: A bug fix
- `docs`: Documentation only changes
- `style`: Code style changes (formatting, etc., that don't affect code execution)
- `refactor`: Code refactoring
- `perf`: Performance improvements
- `test`: Adding or updating tests
- `chore`: Changes to build process or auxiliary tools

**Example**:

```
feat(syscall): add epoll system call support

Implement epoll_create, epoll_ctl, and epoll_wait system calls,
supporting both edge-triggered and level-triggered modes.

Closes #123
```

#### Maintain Linear History

- **Rebasing guidance**: When you need to sync your feature branch with the main branch's progress, use `rebase` instead of `merge` (see below). However, avoid repeatedly rebasing your feature branch during active development, as excessive rebasing can make history harder to follow. We will eventually use squash merge.

```bash
git fetch upstream  # or origin
git rebase upstream/main
```

### 6. Create a Pull Request

#### PR Size

**Please keep PRs as small as possible** to make them easier to review and maintain. A PR should focus on one specific feature or fix.

#### PR Description

When creating a PR, please:

1. Fill in detailed information using the PR template
2. Link related Issues (if applicable)
3. Describe the motivation and implementation approach
4. Explain how to test these changes

#### PR Checklist

Before submitting a PR, please ensure:

- [ ] Code passes `cargo clippy` checks
- [ ] Code is formatted with `cargo fmt`
- [ ] Commit messages follow Conventional Commits specification
- [ ] PR is as small as possible for easier review
- [ ] Updated relevant documentation (if applicable)
- [ ] Added tests (if applicable)

## Code Style

### Rust Code Style

- Follow the official Rust style guide
- Format code using the project's `rustfmt.toml` configuration
- Follow clippy suggestions

### Naming Conventions

- Use meaningful variable and function names
- Follow Rust naming conventions (snake_case for functions and variables, PascalCase for types)

### Comments

- Add documentation comments for public APIs
- Add explanatory comments for complex logic
- You can use either Chinese or English comments, but please be consistent

## Reporting Issues

If you find a bug or have a feature suggestion, please:

1. Check if there's already a related Issue
2. If not, create a new Issue
3. Use the Issue template to provide detailed information

## Getting Help

If you encounter problems while contributing:

1. Check existing Issues and discussions
2. Ask questions in [Discussions](https://github.com/Starry-OS/StarryOS/discussions)
3. Create an Issue to seek help

## Code of Conduct

Please maintain a respectful and professional communication style. We are committed to providing a friendly and inclusive environment for all contributors.

## License

By contributing code, you agree that your contributions will be licensed under the Apache License 2.0.

---

Thank you again for contributing to StarryOS!
