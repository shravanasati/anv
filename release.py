#! /usr/bin/env python3
import subprocess
import sys
import tomllib
from pathlib import Path

def get_update_version(curr: str, bump: str):
	curr_split = curr.split(".")
	if len(curr_split) != 3:
		raise ValueError("version not in semantic x.y.z form")

	idx = 0
	if bump == "patch":
		idx = 2
	elif bump == "minor":
		idx = 1
	elif bump == "major":
		idx = 0
	else:
		raise ValueError("Invalid bump type")

	curr_split[idx] = str(int(curr_split[idx]) + 1)

	for i in range(idx + 1, len(curr_split)):
		curr_split[i] = "0"

	return ".".join(curr_split)

def execute_git_tag(new_version: str):
	try:
		subprocess.run(["git", "tag", new_version], check=True)

	except subprocess.CalledProcessError as e:
		print("==> Failed to create git tag")
		print(e)
		exit(1)
	
def execute_git_push():
	try:
		subprocess.run(["git", "push", "--tags"], check=True)

	except subprocess.CalledProcessError as e:
		print("==> Failed to push git tag")
		print(e)
		exit(1)

CARGO_TOML = Path(__file__).parent / "Cargo.toml"

if not CARGO_TOML.exists():
	print("==> Cargo.toml not found")
	exit(1)

with CARGO_TOML.open("rb") as f:
	data = tomllib.load(f)


if len(sys.argv) != 2:
	print("==> Usage: python release.py <patch|minor|major>")
	exit(1)

bump = sys.argv[1].lower().strip()
if bump not in ("patch", "minor", "major"):
	print("==> Usage: python release.py <patch|minor|major>")
	exit(1)

new_version = get_update_version(data["package"]["version"], bump)

data["package"]["version"] = new_version

with CARGO_TOML.open("wb") as f:
	tomllib.dump(data, f)

print(f"==> Cargo.toml updated with {new_version}")

print("==> Executing git tag")
execute_git_tag(new_version)

print("==> Executing git push")
execute_git_push()