""" Scripts to generate bindings of different python interperator versions

Requires bindgen to be installed (cargo install bindgen), and probably needs a nightly
compiler with rustfmt-nightly.

Also requires a git repo of cpython to be checked out somewhere. As a hack, this can
also build different versions of cpython for testing out
"""
import argparse
import os
import sys


def build_python(cpython_path, version):
    # TODO: probably easier to use pyenv for this?
    print("Compiling python %s from repo at %s" % (version, cpython_path))
    install_path = os.path.join(cpython_path, version)

    ret = os.system(f"""
        cd {cpython_path}
        git checkout {version}

        # build in a subdirectory
        mkdir -p build_{version}
        cd build_{version}
        ../configure prefix={install_path}
        make
        make install
    """)
    if ret:
        return ret

    # also install setuptools_rust/wheel here for building packages
    pip = os.path.join(install_path, "bin", "pip3" if version.startswith("v3") else "pip")
    return os.system(f"{pip} install setuptools_rust wheel")


def extract_bindings(cpython_path, version, configure=False):
    print("Generating bindings for python %s from repo at %s" % (version, cpython_path))
    return os.system(f"""
        cd {cpython_path}
        git checkout {version}

        # need to run configure on the current branch to generate pyconfig.h sometimes
        {("./configure prefix=" + os.path.join(cpython_path, version)) if configure else ""}

        cat include/Python.h > bindgen_input.h
        cat include/frameobject.h >> bindgen_input.h

        bindgen  bindgen_input.h -o bindgen_output.rs \
            --with-derive-default \
            --no-layout-tests --no-doc-comments \
            --whitelist-type PyInterpreterState \
            --whitelist-type PyFrameObject \
            --whitelist-type PyThreadState \
            --whitelist-type PyCodeObject \
            --whitelist-type PyVarObject \
            --whitelist-type PyBytesObject \
            --whitelist-type PyASCIIObject \
            --whitelist-type PyUnicodeObject \
            --whitelist-type PyCompactUnicodeObject \
            --whitelist-type PyStringObject \
             -- -I . -I ./Include
    """)

    # write the file out to the appropiate place, disabling some warnings
    with open(os.path.join("src", "python_bindings", version.replace(".", "_") + ".rs"), "w") as o:
        o.write(f"// Generated bindings for python {version}\n")
        o.write("#![allow(dead_code)]\n")
        o.write("#![allow(non_upper_case_globals)]\n")
        o.write("#![allow(non_camel_case_types)]\n")
        o.write("#![allow(non_snake_case)]\n")
        o.write("#![cfg_attr(feature = \"cargo-clippy\", allow(useless_transmute))]\n")
        o.write("#![cfg_attr(feature = \"cargo-clippy\", allow(default_trait_access))]\n")
        o.write("#![cfg_attr(feature = \"cargo-clippy\", allow(cast_lossless))]\n")
        o.write("#![cfg_attr(feature = \"cargo-clippy\", allow(trivially_copy_pass_by_ref))]\n\n")
        o.write(open(os.path.join(cpython_path, "bindgen_output.rs")).read())


if __name__ == "__main__":
    default_cpython_path = os.path.join(os.getenv("HOME"), "code", "cpython")

    parser = argparse.ArgumentParser(description="runs bindgen on cpython version",
                                     formatter_class=argparse.ArgumentDefaultsHelpFormatter)
    parser.add_argument("--cpython", type=str, default=default_cpython_path,
                        dest="cpython", help="path to cpython repo")
    parser.add_argument("--configure",
                        help="Run configure script prior to generating bindings",
                        action="store_true")
    parser.add_argument("--build",
                        help="Build python for this version",
                        action="store_true")
    parser.add_argument("--all",
                        help="Build all versions",
                        action="store_true")

    parser.add_argument("versions", type=str, nargs='*', help='versions to extract')

    args = parser.parse_args()

    if not os.path.isdir(args.cpython):
        print(f"Directory '{args.cpython}' doesn't exist!")
        print("Pass a valid cpython path in with --cpython <pathname>")
        sys.exit(1)

    if args.all:
        versions = ['v3.7.0', 'v3.6.6', 'v3.5.5', 'v3.4.8', 'v3.3.7', 'v3.2.6', 'v2.7.15']
    else:
        versions = args.versions
        if not versions:
            print("You must specify versions of cpython to generate bindings for, or --all\n")
            parser.print_help()

    for version in versions:
        if args.build:
            # todo: this probably shoudl be a separate script
            if build_python(args.cpython, version):
                print("Failed to build python")
        else:
            if extract_bindings(args.cpython, version, configure=args.configure):
                print("Failed to generate bindings")
