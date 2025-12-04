# Contributing Guide

Thanks for your help improving the project! We are so happy to have you! :tada:

## Code of Conduct

We adhere to [Rust Code of Conduct](https://www.rust-lang.org/policies/code-of-conduct). Please maintain a respectful and professional communication style. We are committed to providing a friendly and inclusive environment for all contributors.

## Getting Help

If you encounter problems while contributing:

1. Check existing issues and discussions
2. Ask questions in [Discussions](https://github.com/Starry-OS/StarryOS/discussions)
3. Create an issue to seek help

## Reporting Issues

If you find a bug or have a feature suggestion, please:

1. Check if there's already a related issue
2. If not, create a new issue
3. Use the issue template to provide detailed information

## Submitting Pull Requests

We welcome any code contributions. It's always welcome and recommended to open an issue to discuss on major changes before opening a PR.

Here are the recommended workflow and guidelines for contributing code:

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
git checkout -b feat/your-feature-name
# or
git checkout -b fix/your-bug-fix
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

The project uses a custom `rustfmt.toml` configuration.

#### Comments

- Add documentation comments for public APIs
- Add explanatory comments for complex logic
- Use English for all comments and documentation

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

- **Rebasing guidance**: When you need to sync your feature branch with the main branch's progress, use `rebase` instead of `merge` (see below). However, avoid repeatedly rebasing your PR branch during active development, as excessive rebasing can make history harder to follow. We will eventually use squash merge.

```bash
git fetch upstream  # or origin
git rebase upstream/main
```

### 6. Create a Pull Request

**Please keep PRs as small as possible** to make them easier to review and maintain. A PR should focus on one specific feature or fix.

#### PR Description

When creating a PR, please:

1. Fill in detailed information using the PR template
2. Link related Issues (if applicable)
3. Describe the motivation and implementation approach
4. Explain how to test these changes

## License

By contributing code, you agree that your contributions will be licensed under the Apache License 2.0.

---

Thank you again for contributing to StarryOS!
