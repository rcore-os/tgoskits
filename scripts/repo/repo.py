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
import shutil
import tempfile
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
        repo_name = self.url.rstrip('/').split('/')[-1]
        if repo_name.endswith('.git'):
            return repo_name[:-4]
        return repo_name


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
    def _run_command(cmd: List[str], check: bool = True) -> subprocess.CompletedProcess:
        """Run a shell command and return the result."""
        print(f"Running: {' '.join(cmd)}")
        result = subprocess.run(cmd, check=check, capture_output=False, text=True)
        return result

    @staticmethod
    def _run_command_with_stdout(cmd: List[str], check: bool = True) -> str:
        """Run a command, keep stderr visible, and return stripped stdout."""
        print(f"Running: {' '.join(cmd)}")
        result = subprocess.run(
            cmd,
            check=check,
            stdout=subprocess.PIPE,
            text=True
        )
        return result.stdout.strip()

    @staticmethod
    def _git_dir() -> Path:
        """Return the path to the current git directory."""
        return Path(
            GitSubtreeManager._run_command_with_stdout(['git', 'rev-parse', '--git-dir'])
        )

    @staticmethod
    def _clear_subtree_cache() -> None:
        """Remove leftover git-subtree cache directories before a split."""
        cache_dir = GitSubtreeManager._git_dir() / 'subtree-cache'
        if cache_dir.exists():
            print(f"Clearing git subtree cache: {cache_dir}")
            shutil.rmtree(cache_dir, ignore_errors=True)

    @staticmethod
    def _split_subtree_rev(target_dir: str) -> Tuple[str, Optional[Path]]:
        """Return the split commit and optional repo path to push from."""
        split_cmd = [
            'git', 'subtree', 'split',
            '--quiet',
            '--prefix=' + target_dir,
        ]

        for attempt in range(2):
            GitSubtreeManager._clear_subtree_cache()
            try:
                split_rev = GitSubtreeManager._run_command_with_stdout(split_cmd)
                if split_rev:
                    return split_rev, None
            except subprocess.CalledProcessError as exc:
                if attempt == 0:
                    print(
                        f"git subtree split failed for '{target_dir}', "
                        "retrying after clearing subtree cache...",
                        file=sys.stderr
                    )
                    print(f"Original split error: {exc}", file=sys.stderr)
                    continue

                print(
                    f"git subtree split still failed for '{target_dir}', "
                    "falling back to a temporary filter-branch split...",
                    file=sys.stderr
                )
                print(f"Original split error: {exc}", file=sys.stderr)

        head_rev = GitSubtreeManager._run_command_with_stdout(['git', 'rev-parse', 'HEAD'])
        temp_dir = Path(tempfile.mkdtemp(prefix='git-subtree-split-'))
        try:
            GitSubtreeManager._run_command(['git', 'clone', '--quiet', '.', str(temp_dir)])
            GitSubtreeManager._run_command([
                'git', '-C', str(temp_dir), 'checkout', '--quiet', head_rev
            ])
            print(
                f"Running: git -C {temp_dir} "
                f"filter-branch -f --subdirectory-filter {target_dir} HEAD"
            )
            subprocess.run(
                [
                    'git',
                    '-C',
                    str(temp_dir),
                    'filter-branch',
                    '-f',
                    '--subdirectory-filter',
                    target_dir,
                    'HEAD',
                ],
                check=True,
                env={**os.environ, 'FILTER_BRANCH_SQUELCH_WARNING': '1'},
                text=True
            )
            split_rev = GitSubtreeManager._run_command_with_stdout([
                'git', '-C', str(temp_dir), 'rev-parse', 'HEAD'
            ])
            if split_rev:
                return split_rev, temp_dir
        except Exception:
            shutil.rmtree(temp_dir, ignore_errors=True)
            raise

        raise ValueError(f"Failed to split subtree at '{target_dir}'")

    @staticmethod
    def get_repo_name(url: str) -> str:
        """Extract repo name from URL."""
        repo_name = url.rstrip('/').split('/')[-1]
        if repo_name.endswith('.git'):
            return repo_name[:-4]
        return repo_name

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

    def resolve_branch(self, url: str, branch: str = "") -> str:
        """Resolve the effective branch for a repository."""
        if branch:
            return branch

        remote_name = f"resolve_{int(time.time())}_{os.getpid()}"
        try:
            subprocess.run(
                ['git', 'remote', 'add', remote_name, url],
                capture_output=True,
                check=True,
                text=True
            )
            subprocess.run(
                ['git', 'fetch', remote_name, '--no-tags'],
                capture_output=True,
                check=True,
                text=True
            )
            branch = self.detect_branch(url, remote_name)
            print(f"Auto-detected branch: {branch}")
            return branch
        finally:
            subprocess.run(['git', 'remote', 'remove', remote_name], capture_output=True)

    @staticmethod
    def current_branch_name() -> str:
        """Return the current local branch name."""
        branch = GitSubtreeManager._run_command_with_stdout(
            ['git', 'rev-parse', '--abbrev-ref', 'HEAD']
        )
        if branch == 'HEAD':
            raise ValueError(
                "Detached HEAD is not supported for subtree history repair. "
                "Please check out a branch first."
            )
        return branch

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

        # Push to the component dev branch by default.
        if branch == "":
            branch = PUSH_DEFAULT_BRANCH
            print(f"Using default push branch: {branch}")

        # Some git-subtree versions fail inside `split`/`push`, so always
        # perform the split explicitly and push the resulting ref ourselves.
        split_rev, push_repo = self._split_subtree_rev(target_dir)
        try:
            cmd = ['git']
            if push_repo is not None:
                cmd.extend(['-C', str(push_repo)])
            cmd.extend([
                'push',
                url,
                f'{split_rev}:refs/heads/{branch}'
            ])
            if force:
                cmd.insert(len(cmd) - 2, '--force')
            self._run_command(cmd)
        finally:
            if push_repo is not None:
                shutil.rmtree(push_repo, ignore_errors=True)

    def repair_subtree_history(
        self,
        repo_name: str,
        url: str,
        target_dir: str,
        branch: str = "",
        output_branch: str = "",
        apply: bool = False,
    ) -> Tuple[str, str]:
        """Rewrite the current branch with a fresh subtree baseline on a new branch."""
        if not self.is_added(target_dir):
            raise ValueError(f"Subtree at '{target_dir}' not found. Cannot repair history.")
        if not self.check_working_tree_clean():
            raise ValueError(
                "Working tree has uncommitted changes. "
                "Please commit or stash your changes before repairing subtree history."
            )

        current_branch = self.current_branch_name()
        head_rev = self._run_command_with_stdout(['git', 'rev-parse', 'HEAD'])
        effective_branch = self.resolve_branch(url, branch)
        timestamp = time.strftime('%Y%m%d-%H%M%S')
        backup_branch = f'backup/{current_branch}-{repo_name}-{timestamp}'
        repaired_branch = output_branch or f'repair/{repo_name}-{timestamp}'

        snapshot_dir = Path(tempfile.mkdtemp(prefix='git-subtree-snapshot-'))
        temp_dir = Path(tempfile.mkdtemp(prefix='git-subtree-repair-'))

        try:
            snapshot_target = snapshot_dir / 'tree'
            shutil.copytree(target_dir, snapshot_target)

            self._run_command(['git', 'clone', '--quiet', '.', str(temp_dir)])
            self._run_command(['git', '-C', str(temp_dir), 'checkout', '--quiet', head_rev])

            index_filter = f"git rm -r --cached --ignore-unmatch -- {shlex.quote(target_dir)}"
            print(
                "Running: "
                f"git -C {temp_dir} filter-branch -f --index-filter {index_filter} --prune-empty HEAD"
            )
            subprocess.run(
                [
                    'git',
                    '-C',
                    str(temp_dir),
                    'filter-branch',
                    '-f',
                    '--index-filter',
                    index_filter,
                    '--prune-empty',
                    'HEAD',
                ],
                check=True,
                env={**os.environ, 'FILTER_BRANCH_SQUELCH_WARNING': '1'},
                text=True
            )

            self._run_command([
                'git',
                '-C',
                str(temp_dir),
                'subtree',
                'add',
                '--prefix=' + target_dir,
                url,
                effective_branch,
                '-m',
                f'Re-add subtree {repo_name} after history repair'
            ])

            shutil.rmtree(temp_dir / target_dir, ignore_errors=True)
            shutil.copytree(snapshot_target, temp_dir / target_dir)
            self._run_command(['git', '-C', str(temp_dir), 'add', target_dir])

            diff_result = subprocess.run(
                ['git', '-C', str(temp_dir), 'diff', '--cached', '--quiet'],
                check=False
            )
            if diff_result.returncode != 0:
                self._run_command([
                    'git',
                    '-C',
                    str(temp_dir),
                    'commit',
                    '-m',
                    f'Restore current {repo_name} subtree state after history repair'
                ])

            self._run_command(['git', 'branch', backup_branch, head_rev])
            self._run_command([
                'git',
                'fetch',
                '--quiet',
                str(temp_dir),
                f'HEAD:refs/heads/{repaired_branch}'
            ])

            if apply:
                self._run_command(['git', 'reset', '--hard', repaired_branch])

            return backup_branch, repaired_branch
        finally:
            shutil.rmtree(snapshot_dir, ignore_errors=True)
            shutil.rmtree(temp_dir, ignore_errors=True)

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
            print(f"\nPushing {repo.repo_name}...")
            if args.force:
                print("Using force mode (will force-push subtree history)")
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


def cmd_repair(args: argparse.Namespace) -> int:
    """Handle the 'repair' command."""
    csv_manager = CSVManager(args.csv)
    git_manager = GitSubtreeManager(csv_manager)

    if not args.repo_name:
        print("Error: repo_name is required", file=sys.stderr)
        return 1

    repo = csv_manager.find_repo(args.repo_name)
    if not repo:
        print(f"Error: Repository '{args.repo_name}' not found", file=sys.stderr)
        return 1

    if not repo.target_dir:
        print(f"Error: Repository '{args.repo_name}' has no target_dir set", file=sys.stderr)
        return 1

    branch = args.branch if args.branch else repo.branch

    try:
        print(f"\nRepairing subtree history for {repo.repo_name}...")
        backup_branch, repaired_branch = git_manager.repair_subtree_history(
            repo_name=repo.repo_name,
            url=repo.url,
            target_dir=repo.target_dir,
            branch=branch,
            output_branch=args.output_branch,
            apply=args.apply
        )
        print("\nRepair completed successfully.")
        print(f"  Backup branch:   {backup_branch}")
        print(f"  Repaired branch: {repaired_branch}")
        if args.apply:
            print(f"  Current branch reset to repaired history: {repaired_branch}")
        else:
            print(f"  Inspect with: git log --oneline {repaired_branch}")
            print(f"  Switch with:  git checkout {repaired_branch}")
    except (subprocess.CalledProcessError, ValueError) as e:
        print(f"Error repairing {repo.repo_name}: {e}", file=sys.stderr)
        return 1

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
  %(prog)s repair repo_name
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
    push_parser = subparsers.add_parser('push', help='Push local changes to remote')
    push_parser.add_argument('repo_name', nargs='?', help='Repository name (or use --all)')
    push_parser.add_argument('--all', action='store_true', help='Push all repositories')
    push_parser.add_argument('-b', '--branch', default='',
                             help=f'Branch name (default: {PUSH_DEFAULT_BRANCH})')
    push_parser.add_argument('-f', '--force', action='store_true',
                             help='Force push subtree history to remote')

    # Repair command
    repair_parser = subparsers.add_parser(
        'repair',
        help='Rewrite subtree history and create a clean repaired branch'
    )
    repair_parser.add_argument('repo_name', help='Repository name')
    repair_parser.add_argument('-b', '--branch', default='', help='Remote branch name')
    repair_parser.add_argument(
        '-o', '--output-branch', default='',
        help='Name for the repaired branch (default: repair/<repo>-<timestamp>)'
    )
    repair_parser.add_argument(
        '--apply',
        action='store_true',
        help='Reset the current branch to the repaired branch after it is created'
    )

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
        'repair': cmd_repair,
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
