#!/usr/bin/env python3
"""
Git Subtree Manager - Manage git subtree repositories using CSV configuration

This script provides commands to add, remove, pull, and push git subtrees
based on a CSV configuration file.
"""

import csv
import os
import sys
import argparse
import subprocess
import re
from pathlib import Path
from typing import Optional, List, Dict, Tuple
from dataclasses import dataclass, field, astuple


# Default paths
CSV_PATH = Path(__file__).parent / "repos.csv"
PUSH_DEFAULT_BRANCH = "dev"


@dataclass
class Repo:
    """Repository configuration entry."""
    url: str
    branch: str = ""
    target_dir: str = ""
    category: str = ""
    description: str = ""

    def __iter__(self):
        return iter(astuple(self))

    @property
    def repo_name(self) -> str:
        """Extract repo name from URL."""
        name = self.url.rstrip('/').split('/')[-1]
        if name.endswith('.git'):
            name = name[:-4]
        return name


class CSVManager:
    """Manages CSV file operations for repository configurations."""

    def __init__(self, csv_path: Path = CSV_PATH):
        self.csv_path = csv_path
        self._repos: Optional[List[Repo]] = None

    def load_repos(self) -> List[Repo]:
        """Load repositories from CSV file."""
        if self._repos is not None:
            return self._repos

        repos = []
        if not self.csv_path.exists():
            return repos

        with open(self.csv_path, 'r', newline='', encoding='utf-8') as f:
            reader = csv.DictReader(f)
            for row in reader:
                repos.append(Repo(
                    url=row.get('url', ''),
                    branch=row.get('branch', ''),
                    target_dir=row.get('target_dir', ''),
                    category=row.get('category', ''),
                    description=row.get('description', '')
                ))
        self._repos = repos
        return repos

    def save_repos(self, repos: Optional[List[Repo]] = None) -> None:
        """Save repositories to CSV file."""
        if repos is not None:
            self._repos = repos
        elif self._repos is None:
            self._repos = []

        with open(self.csv_path, 'w', newline='', encoding='utf-8') as f:
            writer = csv.writer(f)
            writer.writerow(['url', 'branch', 'target_dir', 'category', 'description'])
            for repo in self._repos:
                writer.writerow(list(repo))

    def add_repo(self, url: str, target_dir: str, branch: str = "",
                 category: str = "", description: str = "", skip_if_exists: bool = False) -> bool:
        """Add a new repository entry to the CSV. Returns True if added, False if already exists."""
        repos = self.load_repos()

        # Check for duplicate URL or target_dir
        for repo in repos:
            if repo.url == url:
                # URL matches, verify branch and target_dir also match
                differences = []
                if repo.branch != branch:
                    existing_branch = repo.branch if repo.branch else "main"
                    new_branch = branch if branch else "main"
                    differences.append(f"branch (existing: {existing_branch}, new: {new_branch})")
                if repo.target_dir != target_dir:
                    differences.append(f"target_dir (existing: {repo.target_dir}, new: {target_dir})")

                if differences:
                    raise ValueError(
                        f"Repository with URL '{url}' already exists but has different "
                        f"configuration: {', '.join(differences)}"
                    )

                # All fields match, skip
                if skip_if_exists:
                    return False
                raise ValueError(f"Repository with URL '{url}' already exists")

            if repo.target_dir == target_dir:
                # target_dir matches but URL is different
                if repo.url != url:
                    raise ValueError(
                        f"Repository with target_dir '{target_dir}' already exists "
                        f"with different URL (existing: {repo.url}, new: {url})"
                    )
                if skip_if_exists:
                    return False
                raise ValueError(f"Repository with target_dir '{target_dir}' already exists")

        new_repo = Repo(
            url=url,
            branch=branch,
            target_dir=target_dir,
            category=category,
            description=description
        )
        repos.append(new_repo)
        self.save_repos(repos)
        return True

    def remove_repo(self, repo_name: str) -> Repo:
        """Remove a repository entry by repo name. Returns the removed repo."""
        repos = self.load_repos()

        for i, repo in enumerate(repos):
            if repo.repo_name.lower() == repo_name.lower():
                removed = repos.pop(i)
                self.save_repos(repos)
                return removed

        raise ValueError(f"Repository '{repo_name}' not found in CSV")

    def find_repo(self, repo_name: str) -> Optional[Repo]:
        """Find a repository by repo name."""
        repos = self.load_repos()

        for repo in repos:
            if repo.repo_name.lower() == repo_name.lower():
                return repo

        return None

    def list_repos(self) -> List[Repo]:
        """List all repositories."""
        return self.load_repos()

    def update_repo_branch(self, repo_name: str, new_branch: str) -> Repo:
        """Update the branch for a repository. Returns the updated repo."""
        repos = self.load_repos()

        for i, repo in enumerate(repos):
            if repo.repo_name.lower() == repo_name.lower():
                repos[i].branch = new_branch
                self.save_repos(repos)
                return repos[i]

        raise ValueError(f"Repository '{repo_name}' not found in CSV")


class GitSubtreeManager:
    """Manages git subtree operations."""

    def __init__(self, csv_manager: CSVManager):
        self.csv_manager = csv_manager

    @staticmethod
    def _git_subtree_env() -> Optional[Dict]:
        """Return an env dict suitable for running git-subtree directly via bash.
        git-subtree requires GIT_EXEC_PATH to be set and present in PATH."""
        exec_path_result = subprocess.run(
            ['git', '--exec-path'],
            capture_output=True, text=True
        )
        exec_path = exec_path_result.stdout.strip()
        if not exec_path:
            return None
        env = os.environ.copy()
        env['GIT_EXEC_PATH'] = exec_path
        env['PATH'] = exec_path + ':' + env.get('PATH', '')
        return env

    @staticmethod
    def _git_subtree_cmd(subcommand: str, args: List[str]) -> List[str]:
        """Build a git-subtree command run via bash to avoid dash's hardcoded
        recursion limit of 1000, which git-subtree (a #!/bin/sh script) hits
        on repositories with large commit histories."""
        exec_path_result = subprocess.run(
            ['git', '--exec-path'],
            capture_output=True, text=True
        )
        git_subtree_script = Path(exec_path_result.stdout.strip()) / 'git-subtree'
        if git_subtree_script.exists():
            return ['bash', str(git_subtree_script), subcommand] + args
        # Fallback to the standard invocation if the script isn't found
        return ['git', 'subtree', subcommand] + args

    @staticmethod
    def _run_command(cmd: List[str], check: bool = True,
                     env: Optional[Dict] = None) -> subprocess.CompletedProcess:
        """Run a shell command and return the result."""
        print(f"Running: {' '.join(cmd)}", flush=True)
        result = subprocess.run(cmd, check=check, capture_output=False, text=True, env=env)
        return result

    @staticmethod
    def _run_command_with_stdout(cmd: List[str], check: bool = True,
                                 env: Optional[Dict] = None) -> str:
        """Run a command, keep stderr visible, and return stripped stdout."""
        print(f"Running: {' '.join(cmd)}", flush=True)
        result = subprocess.run(
            cmd,
            check=check,
            stdout=subprocess.PIPE,
            text=True,
            env=env,
        )
        return result.stdout.strip()

    @staticmethod
    def get_repo_name(url: str) -> str:
        """Extract repo name from URL."""
        name = url.rstrip('/').split('/')[-1]
        if name.endswith('.git'):
            name = name[:-4]
        return name

    @staticmethod
    def check_working_tree_clean() -> bool:
        """Check if working tree is clean (no uncommitted changes)."""
        result = subprocess.run(
            ['git', 'status', '--porcelain'],
            capture_output=True,
            text=True
        )
        return result.returncode == 0 and not result.stdout.strip()

    def is_added(self, target_dir: str) -> bool:
        """Check if a subtree is already added."""
        path = Path(target_dir)
        if not path.exists():
            return False

        # Check if target_dir is tracked by git
        result = subprocess.run(
            ['git', 'ls-files', '--error-unmatch', str(path)],
            capture_output=True,
            text=True
        )
        return result.returncode == 0

    def detect_branch(self, url: str, remote_name: str) -> str:
        """Auto-detect the default branch for a repository."""
        # Try main first
        result = subprocess.run(
            ['git', 'rev-parse', f'{remote_name}/main'],
            capture_output=True,
            text=True
        )
        if result.returncode == 0:
            return 'main'
        
        # Try master
        result = subprocess.run(
            ['git', 'rev-parse', f'{remote_name}/master'],
            capture_output=True,
            text=True
        )
        if result.returncode == 0:
            return 'master'
        
        # Use default branch from remote
        result = subprocess.run(
            ['git', 'remote', 'show', remote_name],
            capture_output=True,
            text=True
        )
        if result.returncode == 0:
            for line in result.stdout.split('\n'):
                if 'HEAD branch' in line:
                    branch = line.split(':')[1].strip()
                    if branch:
                        return branch
        
        # Fallback to main
        return 'main'

    def add_subtree(self, url: str, target_dir: str, branch: str = "") -> None:
        """Add a new git subtree."""
        if self.is_added(target_dir):
            print(f"Subtree at '{target_dir}' already exists.")
            return

        # Check if working tree is clean
        if not self.check_working_tree_clean():
            raise ValueError(
                "Working tree has uncommitted changes. "
                "Please commit or stash your changes before adding a subtree."
            )

        repo_name = self.get_repo_name(url)
        
        # Add remote temporarily
        remote_name = target_dir.replace('/', '_')
        subprocess.run(['git', 'remote', 'add', remote_name, url], 
                      capture_output=True)
        
        # Fetch from remote (no tags to avoid conflicts)
        print(f"Fetching from {url}...")
        fetch_result = subprocess.run(
            ['git', 'fetch', remote_name, '--no-tags'],
            capture_output=True,
            text=True
        )
        
        if fetch_result.returncode != 0:
            subprocess.run(['git', 'remote', 'remove', remote_name], 
                          capture_output=True)
            raise ValueError(f"Failed to fetch from {url}")
        
        # Auto-detect branch if not specified
        if branch == "":
            branch = self.detect_branch(url, remote_name)
            print(f"Auto-detected branch: {branch}")
        
        # Add subtree using the remote
        cmd = [
            'git', 'subtree', 'add',
            '--prefix=' + target_dir,
            remote_name,
            branch,
            '-m', f'Add subtree {repo_name}'
        ]
        
        try:
            self._run_command(cmd)
        finally:
            # Clean up remote
            subprocess.run(['git', 'remote', 'remove', remote_name], 
                          capture_output=True)

    def pull_subtree(self, url: str, target_dir: str, branch: str = "", force: bool = False) -> None:
        """Pull updates from a git subtree."""
        if not self.is_added(target_dir):
            print(f"Subtree at '{target_dir}' not found. Adding...")
            self.add_subtree(url, target_dir, branch)
            return

        # Auto-detect branch if not specified
        if branch == "":
            remote_name = target_dir.replace('/', '_')
            subprocess.run(['git', 'remote', 'add', remote_name, url], 
                          capture_output=True)
            subprocess.run(['git', 'fetch', remote_name, '--no-tags'], 
                          capture_output=True)
            branch = self.detect_branch(url, remote_name)
            print(f"Auto-detected branch: {branch}")
            subprocess.run(['git', 'remote', 'remove', remote_name], 
                          capture_output=True)

        # Force mode: remove and re-add the subtree
        if force:
            print(f"Force mode: removing '{target_dir}' and re-adding from branch '{branch}'...")
            # Remove the directory
            subprocess.run(['git', 'rm', '-r', '--cached', target_dir], check=True)
            subprocess.run(['rm', '-rf', target_dir])
            # Commit the removal to avoid leaving uncommitted changes
            repo_name = self.get_repo_name(url)
            subprocess.run([
                'git', 'commit', '-m',
                f'Remove subtree {target_dir} before force re-add'
            ], check=True)
            # Re-add the subtree
            self.add_subtree(url, target_dir, branch)
            return

        repo_name = self.get_repo_name(url)
        cmd = [
            'git', 'subtree', 'pull',
            '--prefix=' + target_dir,
            url,
            branch,
            '-m', f'Merge subtree {repo_name}/{branch}'
        ]
        self._run_command(cmd)

    def push_subtree(self, url: str, target_dir: str, branch: str = "", force: bool = False) -> None:
        """Push local changes to a git subtree."""
        if not self.is_added(target_dir):
            raise ValueError(f"Subtree at '{target_dir}' not found. Cannot push.")

        repo_name = self.get_repo_name(url)

        # Push to the component dev branch by default.
        if branch == "":
            branch = PUSH_DEFAULT_BRANCH
            print(f"Using default push branch: {branch}")

        if force:
            # Some git-subtree versions strip the leading '+' from the refspec
            # before invoking `git push`, so perform the split/push explicitly.
            # Use bash to invoke git-subtree to avoid dash's 1000-recursion limit.
            subtree_env = self._git_subtree_env()
            split_cmd = self._git_subtree_cmd('split', [
                '--quiet',
                '--prefix=' + target_dir,
            ])
            split_rev = self._run_command_with_stdout(split_cmd, env=subtree_env)
            if not split_rev:
                raise ValueError(f"Failed to split subtree at '{target_dir}'")

            push_cmd = [
                'git', 'push',
                '--force',
                url,
                f'{split_rev}:refs/heads/{branch}'
            ]
            self._run_command(push_cmd)
            return

        # Use bash to invoke git-subtree to avoid dash's hardcoded 1000-recursion
        # limit, which is hit when the repo history exceeds ~1000 commits.
        subtree_env = self._git_subtree_env()

        # Clear any stale subtree split cache before pushing.  Some git-subtree
        # versions call `git notes add` (not `--force`) when caching a split
        # result, which raises "fatal: cache for <hash> already exists!" if the
        # same commit was previously cached.  Deleting the notes ref beforehand
        # is safe; the cache is purely a performance optimisation and will be
        # rebuilt automatically on the next operation.
        subprocess.run(
            ['git', 'update-ref', '-d', 'refs/notes/subtree-cache'],
            capture_output=True,
        )

        push_args = [
            '--quiet',
            '--prefix=' + target_dir,
            url,
            branch,
        ]
        # axdriver_crates has duplicate subtree join trailers in the shared
        # history.  `git subtree push` scans those trailers before splitting and
        # can fail with "cache for <hash> already exists!" unless we bypass join
        # discovery via --ignore-joins.
        use_ignore_joins = repo_name == 'axdriver_crates'
        if use_ignore_joins:
            print(
                f"Using --ignore-joins for {repo_name} to avoid duplicate subtree history conflicts.",
                flush=True,
            )
            push_args.insert(1, '--ignore-joins')

        cmd = self._git_subtree_cmd('push', push_args)
        try:
            self._run_command(cmd, env=subtree_env)
        except subprocess.CalledProcessError as exc:
            if use_ignore_joins:
                raise

            if exc.returncode != 1:
                raise

            # Other subtrees may hit the same git-subtree history bug after
            # history repairs or duplicate join metadata, so retry once with
            # --ignore-joins when we detect the characteristic cache error.
            if 'cache for ' not in str(exc):
                raise

            print(
                "Retrying git subtree push with --ignore-joins after detecting duplicate subtree cache history.",
                flush=True,
            )
            retry_args = push_args.copy()
            retry_args.insert(1, '--ignore-joins')
            retry_cmd = self._git_subtree_cmd('push', retry_args)
            self._run_command(retry_cmd, env=subtree_env)

    def switch_branch(self, url: str, target_dir: str, old_branch: str, new_branch: str) -> None:
        """Switch a subtree to a different branch."""
        if not self.is_added(target_dir):
            print(f"Subtree at '{target_dir}' not found. Adding...")
            self.add_subtree(url, target_dir, new_branch)
            return

        # Pull from the new branch to get the changes
        repo_name = self.get_repo_name(url)
        cmd = [
            'git', 'subtree', 'pull',
            '--prefix=' + target_dir,
            url,
            new_branch,
            '-m', f'Switch {repo_name} from {old_branch} to {new_branch}'
        ]
        self._run_command(cmd)


def cmd_add(args: argparse.Namespace) -> int:
    """Handle the 'add' command."""
    csv_manager = CSVManager(args.csv)
    git_manager = GitSubtreeManager(csv_manager)

    # Validate required arguments
    if not args.url:
        print("Error: --url is required", file=sys.stderr)
        return 1

    if not args.target:
        print("Error: --target is required", file=sys.stderr)
        return 1

    url = args.url
    target_dir = args.target
    branch = args.branch or ""
    category = args.category or ""
    description = args.description or ""

    # Add to CSV (skip if already exists)
    added = csv_manager.add_repo(url, target_dir, branch, category, description, skip_if_exists=True)
    if added:
        print(f"Added to CSV: {url} -> {target_dir}")
    else:
        print(f"Repository already exists in CSV: {url}")

    # Add git subtree (this will check if already added to git)
    try:
        git_manager.add_subtree(url, target_dir, branch)
        print(f"Successfully added subtree: {target_dir}")
    except subprocess.CalledProcessError as e:
        print(f"Error adding git subtree: {e}", file=sys.stderr)
        return 1

    return 0


def cmd_remove(args: argparse.Namespace) -> int:
    """Handle the 'remove' command."""
    csv_manager = CSVManager(args.csv)

    if not args.repo_name:
        print("Error: repo_name is required", file=sys.stderr)
        return 1

    repo_name = args.repo_name

    # Find and display repo before removing
    repo = csv_manager.find_repo(repo_name)
    if not repo:
        print(f"Error: Repository '{repo_name}' not found", file=sys.stderr)
        return 1

    print(f"Found repository: {repo.repo_name}")
    print(f"  URL: {repo.url}")
    print(f"  Target: {repo.target_dir}")
    print(f"  Category: {repo.category}")

    # Remove from CSV
    try:
        removed = csv_manager.remove_repo(repo_name)
        print(f"Removed '{removed.repo_name}' from CSV")
    except ValueError as e:
        print(f"Error: {e}", file=sys.stderr)
        return 1

    # Ask about removing directory
    if args.force or args.remove_dir:
        target_dir = removed.target_dir
        if target_dir and Path(target_dir).exists():
            try:
                subprocess.run(['git', 'rm', '-r', target_dir], check=True)
                print(f"Removed directory: {target_dir}")
            except subprocess.CalledProcessError as e:
                print(f"Warning: Could not remove directory: {e}", file=sys.stderr)
    else:
        print("Note: The directory still exists. Use --remove-dir to remove it.")

    return 0


def cmd_pull(args: argparse.Namespace) -> int:
    """Handle the 'pull' command."""
    csv_manager = CSVManager(args.csv)
    git_manager = GitSubtreeManager(csv_manager)

    if args.all:
        repos = csv_manager.list_repos()
        if not repos:
            print("No repositories found in CSV")
            return 0
    else:
        if not args.repo_name:
            print("Error: repo_name is required (or use --all)", file=sys.stderr)
            return 1

        repo = csv_manager.find_repo(args.repo_name)
        if not repo:
            print(f"Error: Repository '{args.repo_name}' not found", file=sys.stderr)
            return 1
        repos = [repo]

    # Track skipped repos
    skipped = []

    for repo in repos:
        if not repo.target_dir:
            skipped.append(f"{repo.repo_name} (no target_dir)")
            continue

        # Use command-line branch if specified, otherwise use CSV branch
        branch = args.branch if args.branch else repo.branch

        try:
            print(f"\nPulling {repo.repo_name}...")
            if args.force:
                print(f"Using force mode (will prefer remote changes on conflict)")
            git_manager.pull_subtree(repo.url, repo.target_dir, branch, force=args.force)
        except ValueError as e:
            print(f"Error: {e}", file=sys.stderr)
            if not args.all:
                return 1
        except subprocess.CalledProcessError as e:
            print(f"Error pulling {repo.repo_name}: {e}", file=sys.stderr)
            if not args.all:
                return 1

    if skipped:
        print("\nSkipped repositories:")
        for s in skipped:
            print(f"  - {s}")

    return 0


def cmd_push(args: argparse.Namespace) -> int:
    """Handle the 'push' command."""
    csv_manager = CSVManager(args.csv)
    git_manager = GitSubtreeManager(csv_manager)

    if args.all:
        repos = csv_manager.list_repos()
        if not repos:
            print("No repositories found in CSV")
            return 0
    else:
        if not args.repo_name:
            print("Error: repo_name is required (or use --all)", file=sys.stderr)
            return 1

        repo = csv_manager.find_repo(args.repo_name)
        if not repo:
            print(f"Error: Repository '{args.repo_name}' not found", file=sys.stderr)
            return 1
        repos = [repo]

    # Track skipped repos
    skipped = []

    for repo in repos:
        if not repo.target_dir:
            skipped.append(f"{repo.repo_name} (no target_dir)")
            continue

        # Use command-line branch if specified, otherwise push to the default dev branch.
        branch = args.branch if args.branch else PUSH_DEFAULT_BRANCH

        try:
            print(f"\nPushing {repo.repo_name}...", flush=True)
            if args.force:
                print("Using force mode (will force-push subtree history)", flush=True)
            git_manager.push_subtree(repo.url, repo.target_dir, branch, force=args.force)
        except (subprocess.CalledProcessError, ValueError) as e:
            print(f"Error pushing {repo.repo_name}: {e}", file=sys.stderr)
            if not args.all:
                return 1

    if skipped:
        print("\nSkipped repositories:")
        for s in skipped:
            print(f"  - {s}")

    return 0


def cmd_list(args: argparse.Namespace) -> int:
    """Handle the 'list' command."""
    csv_manager = CSVManager(args.csv)
    git_manager = GitSubtreeManager(csv_manager)
    repos = csv_manager.list_repos()

    if not repos:
        print("No repositories found")
        return 0

    # Filter by category if specified
    if args.category:
        repos = [r for r in repos if r.category.lower() == args.category.lower()]

    # Print header
    print(f"{'Name':<25} {'Category':<15} {'Target':<35} {'Branch':<10}")
    print("-" * 85)

    for repo in repos:
        if repo.branch:
            branch = repo.branch
        elif repo.target_dir:
            # Auto-detect branch from remote
            remote_name = repo.target_dir.replace('/', '_')
            subprocess.run(['git', 'remote', 'add', remote_name, repo.url],
                          capture_output=True)
            subprocess.run(['git', 'fetch', remote_name, '--no-tags'],
                          capture_output=True)
            branch = git_manager.detect_branch(repo.url, remote_name)
            subprocess.run(['git', 'remote', 'remove', remote_name],
                          capture_output=True)
        else:
            branch = "<none>"
        target = repo.target_dir if repo.target_dir else "<not set>"
        category = repo.category if repo.category else "<none>"
        print(f"{repo.repo_name:<25} {category:<15} {target:<35} {branch:<10}")

    print(f"\nTotal: {len(repos)} repositories")
    return 0


def cmd_branch(args: argparse.Namespace) -> int:
    """Handle the 'branch' command."""
    csv_manager = CSVManager(args.csv)
    git_manager = GitSubtreeManager(csv_manager)

    if not args.repo_name:
        print("Error: repo_name is required", file=sys.stderr)
        return 1

    if not args.branch:
        print("Error: branch is required", file=sys.stderr)
        return 1

    repo_name = args.repo_name
    new_branch = args.branch

    # Find the repository
    repo = csv_manager.find_repo(repo_name)
    if not repo:
        print(f"Error: Repository '{repo_name}' not found", file=sys.stderr)
        return 1

    if not repo.target_dir:
        print(f"Error: Repository '{repo_name}' has no target_dir set", file=sys.stderr)
        return 1

    old_branch = repo.branch if repo.branch else "main"

    # Pull from new branch first (only update CSV after success)
    try:
        print(f"Switching {repo_name} to branch '{new_branch}'...")
        git_manager.switch_branch(repo.url, repo.target_dir, old_branch, new_branch)
        print(f"Successfully switched {repo_name} to branch '{new_branch}'")
    except (subprocess.CalledProcessError, ValueError) as e:
        print(f"Error switching branch: {e}", file=sys.stderr)
        # Print original git error if available
        if isinstance(e, subprocess.CalledProcessError) and e.stderr:
            print(f"Git error output: {e.stderr}", file=sys.stderr)
        return 1

    # Update CSV only after successful git operation
    try:
        csv_manager.update_repo_branch(repo_name, new_branch)
        print(f"Updated CSV: {repo_name} branch: {old_branch} -> {new_branch}")
    except ValueError as e:
        print(f"Error updating CSV: {e}", file=sys.stderr)
        return 1

    return 0


def cmd_init(args: argparse.Namespace) -> int:
    """Handle the 'init' command - add subtrees from a CSV file (repos.sh equivalent)."""
    import_csv_path = args.file
    
    if not import_csv_path.exists():
        print(f"Error: CSV file '{import_csv_path}' not found", file=sys.stderr)
        return 1
    
    # Load repositories from the import file
    import_manager = CSVManager(import_csv_path)
    import_repos = import_manager.load_repos()
    
    if not import_repos:
        print("No repositories found in the import CSV file")
        return 0
    
    # Git subtree manager (we don't need CSV manager for this operation)
    csv_manager = CSVManager(args.csv)
    git_manager = GitSubtreeManager(csv_manager)
    
    # Track statistics
    added_count = 0
    skipped_count = 0
    error_count = 0
    
    print(f"Found {len(import_repos)} repositories in {import_csv_path}")
    print("=" * 80)
    
    for repo in import_repos:
        repo_name = repo.repo_name
        target_dir = repo.target_dir
        branch = repo.branch if repo.branch else ""
        
        print(f"\nProcessing: {repo_name}")
        print(f"  URL: {repo.url}")
        print(f"  Target: {target_dir}")
        print(f"  Branch: {branch if branch else 'auto-detect'}")
        
        if not target_dir:
            print(f"  ⚠ Skipped: No target_dir specified")
            skipped_count += 1
            continue
        
        # Check if target directory already exists (like repos.sh does)
        if git_manager.is_added(target_dir):
            print(f"  ⚠ Skipped: Directory '{target_dir}' already exists")
            skipped_count += 1
            continue
        
        # Add git subtree directly (preserving history)
        try:
            git_manager.add_subtree(repo.url, target_dir, branch)
            print(f"  ✓ Successfully added subtree")
            added_count += 1
        except subprocess.CalledProcessError as e:
            print(f"  ✗ Error: {e}")
            error_count += 1
        except ValueError as e:
            print(f"  ✗ Error: {e}")
            error_count += 1

    # Print summary
    print("\n" + "=" * 80)
    print("Summary:")
    print(f"  Added: {added_count}")
    print(f"  Skipped: {skipped_count}")
    print(f"  Errors: {error_count}")

    return 0 if error_count == 0 else 1


def main() -> int:
    """Main entry point."""
    parser = argparse.ArgumentParser(
        description='Git Subtree Manager - Manage git subtrees using CSV configuration',
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  %(prog)s add --url https://github.com/user/repo --target components/repo --branch main
  %(prog)s remove repo_name
  %(prog)s pull --all
  %(prog)s pull repo_name
  %(prog)s push repo_name
  %(prog)s list --category Hypervisor
        """
    )

    parser.add_argument('--csv', type=Path, default=CSV_PATH,
                        help='Path to CSV file (default: repos.csv in script directory)')

    subparsers = parser.add_subparsers(dest='command', help='Available commands')

    # Add command
    add_parser = subparsers.add_parser('add', help='Add a new subtree repository')
    add_parser.add_argument('--url', required=True, help='Repository URL')
    add_parser.add_argument('--target', required=True, help='Target directory path')
    add_parser.add_argument('--branch', default='', help='Branch name (default: main)')
    add_parser.add_argument('--category', default='', help='Category name')
    add_parser.add_argument('--description', default='', help='Repository description')

    # Remove command
    remove_parser = subparsers.add_parser('remove', help='Remove a subtree repository')
    remove_parser.add_argument('repo_name', help='Repository name (extracted from URL)')
    remove_parser.add_argument('--remove-dir', action='store_true',
                               help='Also remove the directory')
    remove_parser.add_argument('-f', '--force', action='store_true',
                               help='Force removal without confirmation')

    # Pull command
    pull_parser = subparsers.add_parser('pull', help='Pull updates from remote')
    pull_parser.add_argument('repo_name', nargs='?', help='Repository name (or use --all)')
    pull_parser.add_argument('--all', action='store_true', help='Pull all repositories')
    pull_parser.add_argument('-b', '--branch', default='', help='Branch name')
    pull_parser.add_argument('-f', '--force', action='store_true',
                            help='Force pull: prefer remote changes on conflict')

    # Push command
    push_parser = subparsers.add_parser(
        'push',
        help='Push local changes to remote',
        description=(
            "Push local subtree changes to the configured remote branch.\n\n"
            "Notes:\n"
            "  - axdriver_crates is pushed with --ignore-joins by default to avoid\n"
            "    duplicate subtree history conflicts.\n"
            "  - Other repositories automatically retry with --ignore-joins if\n"
            "    git-subtree reports the known 'cache for <hash> already exists!'\n"
            "    error while scanning prior subtree joins."
        ),
        formatter_class=argparse.RawTextHelpFormatter,
    )
    push_parser.add_argument('repo_name', nargs='?', help='Repository name (or use --all)')
    push_parser.add_argument('--all', action='store_true', help='Push all repositories')
    push_parser.add_argument('-b', '--branch', default='',
                             help=f'Branch name (default: {PUSH_DEFAULT_BRANCH})')
    push_parser.add_argument('-f', '--force', action='store_true',
                             help='Force push subtree history to remote')

    # List command
    list_parser = subparsers.add_parser('list', help='List all repositories')
    list_parser.add_argument('--category', help='Filter by category')

    # Branch command
    branch_parser = subparsers.add_parser('branch', help='Switch a subtree to a different branch')
    branch_parser.add_argument('repo_name', help='Repository name')
    branch_parser.add_argument('branch', help='New branch name')

    # Init command
    init_parser = subparsers.add_parser('init', help='Initialize subtrees from a CSV file')
    init_parser.add_argument('-f', '--file', required=True, type=Path,
                             help='Path to CSV file containing repositories to import')

    args = parser.parse_args()

    if not args.command:
        parser.print_help()
        return 1

    # Dispatch to command handler
    handlers = {
        'add': cmd_add,
        'remove': cmd_remove,
        'pull': cmd_pull,
        'push': cmd_push,
        'list': cmd_list,
        'branch': cmd_branch,
        'init': cmd_init,
    }

    handler = handlers.get(args.command)
    if handler:
        return handler(args)

    return 1


if __name__ == "__main__":
    sys.exit(main())
