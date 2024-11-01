from collections import defaultdict
import requests
import pathlib
import yaml
import re


_VERSIONS_URL = "https://raw.githubusercontent.com/actions/python-versions/main/versions-manifest.json"  # noqa


def parse_version(v):
    return tuple(int(part) for part in re.split(r"\W", v)[:3])


def get_github_python_versions():
    versions_json = requests.get(_VERSIONS_URL).json()

    # windows platform support isn't great for older versions of python
    # get a map of version: platform/arch so we can exclude here
    platforms = {}
    for v in versions_json:
        platforms[v["version"]] = set((f["platform"], f["arch"]) for f in v["files"])

    raw_versions = [v["version"] for v in versions_json]
    minor_versions = defaultdict(list)

    for version_str in raw_versions:
        if "-" in version_str:
            continue

        major, minor, patch = parse_version(version_str)
        if major == 3 and minor < 6:
            # we don't support python 3.0/3.1/3.2 , and don't bother testing 3.3/3.4/3.5
            continue

        elif major == 2 and minor < 7:
            # we don't test python support before 2.7
            continue
        minor_versions[(major, minor)].append(patch)

    versions = []
    for (major, minor), patches in minor_versions.items():
        patches.sort()

        # for older versions of python, don't test all patches
        # (just test first and last) to keep the test matrix down
        if major == 2 or minor <= 11:
            patches = [patches[0], patches[-1]]

        if major == 3 and minor > 13:
            continue

        versions.extend(f"{major}.{minor}.{patch}" for patch in patches)

    return versions, platforms


def update_python_test_versions():
    versions, platforms = get_github_python_versions()
    versions = sorted(versions, key=parse_version)

    build_yml_path = (
        pathlib.Path(__file__).parent.parent / ".github" / "workflows" / "build.yml"
    )

    build_yml = yaml.safe_load(open(".github/workflows/build.yml"))
    test_matrix = build_yml["jobs"]["test-wheels"]["strategy"]["matrix"]
    existing_python_versions = test_matrix["python-version"]
    if versions == existing_python_versions:
        return

    print("Adding new versions")
    print("Old:", existing_python_versions)
    print("New:", versions)

    # we can't use the yaml package to update the GHA script, since
    # the data in build_yml is treated as an unordered dictionary.
    # instead modify the file in place
    lines = list(open(build_yml_path))
    first_line = lines.index(
        "      # automatically generated by ci/update_python_test_versions.py\n"
    )

    first_version_line = lines.index("          [\n", first_line)
    last_version_line = lines.index("          ]\n", first_version_line)
    new_versions = [f"            {v},\n" for v in versions]
    lines = lines[: first_version_line + 1] + new_versions + lines[last_version_line:]

    # also automatically exclude >= v3.11.* from running on OSX,
    # since it currently fails in GHA on SIP errors
    exclusions = []
    for v in versions:
        # if we don't have a python version for osx/windows skip
        if ("darwin", "x64") not in platforms[v] or v.startswith("3.12"):
            exclusions.append("          - os: macos-13\n")
            exclusions.append(f"            python-version: {v}\n")

        if ("win32", "x64") not in platforms[v]:
            exclusions.append("          - os: windows-latest\n")
            exclusions.append(f"            python-version: {v}\n")

    first_exclude_line = lines.index("        exclude:\n", first_line)
    last_exclude_line = lines.index("\n", first_exclude_line)
    lines = lines[: first_exclude_line + 1] + exclusions + lines[last_exclude_line:]

    with open(build_yml_path, "w") as o:
        o.write("".join(lines))


if __name__ == "__main__":
    update_python_test_versions()
