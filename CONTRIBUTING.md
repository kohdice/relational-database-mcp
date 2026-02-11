# Contributing to relational-database-mcp

Thank you for considering contributing to **relational-database-mcp**!

Your contributions are what make this project great, whether it's through:

- reporting issues
- suggesting features
- submitting pull requests

Together, we can make **relational-database-mcp** even better!

## Table of Contents

1. [Developer Guide](#developer-guide)
   - [Setup](#setup)
   - [Build and Test](#build-and-test)
2. [How to Contribute](#how-to-contribute)
   - [Issues](#issues)
   - [Pull Requests](#pull-requests)
   - [Documentation](#documentation)
3. [Git Commit Guidelines](#git-commit-guidelines)
   - [Type](#type)

## Developer Guide

### Setup

```bash
# With direnv
direnv allow

# Without direnv
nix develop
```

### Build and Test

```bash
zig build test
```

## How to Contribute

### Issues

Feel free to create issues for any bugs, feature requests, or questions.
Please provide as much detail as possible to help others understand the context and the problem.

It is recommended to include the following:

- A clear title and description.
- Steps to reproduce the issue (if applicable).
- Relevant logs, screenshots, or error messages.

### Pull Requests

Pull requests for improvements, bug fixes, or new features are always welcome.
Please follow these steps:

1. Fork the repository and create a new branch.
2. Make your changes and write clear, descriptive commit messages.
3. Test your changes locally to ensure nothing is broken.
4. Submit a pull request and include:
   - A detailed description of your changes.
   - A reference to any related issues (e.g., "Fixes #443").

### Documentation

Contributions to documentation are highly valued.
If you find anything unclear or outdated, please consider improving it or adding a new section if necessary.

## Git Commit Guidelines

### Type

Commit messages must follow one of the following types:

- **feat**: A new feature
- **fix**: A bug fix
- **refactor**: A code change that neither fixes a bug nor adds a feature
- **test**: Adding missing or correcting existing tests
- **style**: Changes that do not affect the meaning of the code (e.g., white-space, formatting, missing semi-colons)
- **chore**: Changes to the build process or auxiliary tools and libraries such as documentation generation
- **docs**: Documentation only changes
- **ci**: Changes to CI configuration files and scripts
- **perf**: A code change that improves performance

---

Thank you for your contributions, big or small, in making **relational-database-mcp** better!
