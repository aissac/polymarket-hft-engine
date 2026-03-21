#!/bin/bash
# Gabagool Bot - GitHub Deploy Script
# Run this after creating the GitHub repo

set -e

REPO=${1:-"DEE/gabagool-bot"}

echo "📤 Pushing to GitHub: $REPO"

# Add remote (if not already set)
if ! git remote get-url origin >/dev/null 2>&1; then
    git remote add origin "https://github.com/$REPO.git"
fi

# Push
git push -u origin master

echo "✅ Pushed! Create PRs for changes."
echo ""
echo "To set up GitHub Actions for CI/CD, add:"
echo "  - Rust build workflow"
echo "  - Docker build on release"
