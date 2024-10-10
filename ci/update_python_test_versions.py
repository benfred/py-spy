from collections import defaultdict
import requests
import pathlib
import re


_VERSIONS_URL = "https://raw.githubusercontent.com/actions/python-versions/main/versions-manifest.json"  # noqa


def parse_version(v):
    return tuple(int(part) for part in re.split("\W", v)[:3])


def get_github_python_versions():
    versions_json = requests.get(_VERSIONS_URL).json()
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
        if (major == 2 or minor < 10):
            patches = [patches[0], patches[-1]]

        if (major == 3 and minor >= 12):
            continue

        versions.extend(f"{major}.{minor}.{patch}" for patch in patches)

    return versions


if __name__ == "__main__":
    versions = sorted(
        get_github_python_versions(), key=parse_version)
    build_yml = (
        pathlib.Path(__file__).parent.parent / ".github" / "workflows" / "build.yml"
    )


    transformed = []
    for line in open(build_yml):
        if line.startswith("        python-version: ["):
            newversions = f"        python-version: [{', '.join(v for v in versions)}]\n"
            if newversions != line:
                print("Adding new versions")
                print("Old:", line)
                print("New:", newversions)
            line = newversions
        transformed.append(line)

    # also automatically exclude v3.11.* from running on OSX,
    # since it currently fails in GHA on SIP errors
    exclusions = []
    for v in versions:
        if v.startswith("3.11"):
            exclusions.append("          - os: macos-13\n")
            exclusions.append(f"            python-version: {v}\n")
    test_wheels = transformed.index("  test-wheels:\n")
    first_line = transformed.index("        exclude:\n", test_wheels)
    last_line = transformed.index("\n", first_line)
    transformed = transformed[:first_line+1] + exclusions + transformed[last_line:]

    with open(build_yml, "w") as o:
        o.write("".join(transformed))
