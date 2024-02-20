import requests
import pathlib
import re


_VERSIONS_URL = "https://raw.githubusercontent.com/actions/python-versions/main/versions-manifest.json"  # noqa


def parse_version(v):
    return tuple(int(part) for part in re.split("\W", v)[:3])


def get_github_python_versions():
    versions_json = requests.get(_VERSIONS_URL).json()
    raw_versions = [v["version"] for v in versions_json]
    versions = []
    for version_str in raw_versions: 
        if "-" in version_str and version_str != "3.11.0-beta.5":
            continue

        major, minor, patch = parse_version(version_str)
        if major == 3 and minor < 5:
            # we don't support python 3.0/3.1/3.2 , and don't bother testing 3.3/3.4
            continue

        elif major == 2 and minor < 7:
            # we don't test python support before 2.7
            continue

        versions.append(version_str)
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

    with open(build_yml, "w") as o:
        o.write("".join(transformed))
