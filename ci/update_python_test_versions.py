import requests
import pkg_resources
import pathlib


_VERSIONS_URL = "https://raw.githubusercontent.com/actions/python-versions/main/versions-manifest.json"  # noqa


def get_github_python_versions():
    versions_json = requests.get(_VERSIONS_URL).json()
    raw_versions = [v["version"] for v in versions_json]
    versions = []
    for version_str in raw_versions:
        if '-' in version_str:
            continue

        v = pkg_resources.parse_version(version_str)
        if v.major == 3 and v.minor < 3:
            # we don't support python 3.0/3.1/3.2
            continue

        elif v.major == 2 and v.minor < 3:
            # we don't support python before 2.3
            continue

        versions.append(version_str)
    return versions


if __name__ == "__main__":
    versions = sorted(get_github_python_versions(), key = lambda x: pkg_resources.parse_version(x))
    build_yml = pathlib.Path(__file__).parent.parent / ".github" / "workflows" / "build.yml"

    transformed = []
    for line in open(build_yml):
        if line.startswith("        python-version: ["):
            print(line)
            line = f"        python-version: [{', '.join(v for v in versions)}]\n"
            print(line)
        transformed.append(line)

    with open(build_yml, "w") as o:
        o.write("".join(transformed))
