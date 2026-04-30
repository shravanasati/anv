#! /usr/bin/env python3
import subprocess
import sys
import tomlkit
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

def execute(cmd: list[str], err_msg: str):
	try:
		subprocess.run(cmd, check=True)
	except subprocess.CalledProcessError as e:
		print(f"==> {err_msg}")
		print(e)
		exit(1)

CARGO_TOML = Path(__file__).parent / "Cargo.toml"

if not CARGO_TOML.exists():
	print("==> Cargo.toml not found")
	exit(1)

with CARGO_TOML.open("rb") as f:
	data = tomlkit.load(f)


if len(sys.argv) != 2:
	print("==> Usage: python release.py <patch|minor|major>")
	exit(1)

bump = sys.argv[1].lower().strip()
if bump not in ("patch", "minor", "major"):
	print("==> Usage: python release.py <patch|minor|major>")
	exit(1)

curr_branch = subprocess.run(["git", "branch", "--show-current"], check=True, capture_output=True).stdout.decode().strip()

if curr_branch != "dev":
	print("==> Not on dev branch, aborting")
	exit(1)

new_version = get_update_version(data["package"]["version"], bump)

ans = input(f"{data["package"]["version"]} --> {new_version}, go ahead (y/n)? ").strip().lower()

if ans != "y":
	exit(0)

data["package"]["version"] = new_version

with CARGO_TOML.open("w") as f:
	tomlkit.dump(data, f)

print(f"==> Cargo.toml updated with {new_version}")

print("==> Executing `cargo update --workspace`")
execute(["cargo", "update", "--workspace"], "Failed to execute `cargo update --workspace`")

print("==> Committing changes")
execute(["git", "commit", "-am", f"release: v{new_version}"], "Failed to create git commit")

print("==> Executing git tag")
execute(["git", "tag", f"v{new_version}"], "Failed to create git tag")

input("Press enter to push to remote")
print("==> Executing git push")
execute(["git", "push", "--atomic", "origin", "dev", "--tags"], "Failed to push git tag")

print("Watch the release build at https://github.com/shravanasati/anv/actions")