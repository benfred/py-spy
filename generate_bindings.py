""" Scripts to generate bindings of different python interpreter versions

Requires bindgen to be installed (cargo install bindgen), and probably needs a nightly
compiler with rustfmt-nightly.

Also requires a git repo of cpython to be checked out somewhere. As a hack, this can
also build different versions of cpython for testing out
"""
import argparse
import os
import sys
import tempfile


def build_python(cpython_path, version):
    # TODO: probably easier to use pyenv for this?
    print("Compiling python %s from repo at %s" % (version, cpython_path))
    install_path = os.path.abspath(os.path.join(cpython_path, version))

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


def calculate_pyruntime_offsets(cpython_path, version, configure=False):
    ret = os.system(f"""cd {cpython_path} && git checkout {version}""")
    if ret:
        return ret

    if configure:
        os.system(f"cd {cpython_path} && ./configure prefix=" + os.path.abspath(os.path.join(cpython_path, version)))

    # simple little c program to get the offsets we need from the pyruntime struct
    # (using rust bindgen here is more complicated than necessary)
    program = r"""
        #include <stddef.h>
        #include <stdio.h>
        #define Py_BUILD_CORE 1
        #include "Include/Python.h"
        #include "Include/internal/pystate.h"

        int main(int argc, const char * argv[]) {
            size_t interp_head = offsetof(_PyRuntimeState, interpreters.head);
            printf("pub static INTERP_HEAD_OFFSET: usize = %i;\n", interp_head);

            size_t tstate_current = offsetof(_PyRuntimeState, gilstate.tstate_current);
            printf("pub static TSTATE_CURRENT: usize = %i;\n", tstate_current);
        }
    """

    if not os.path.isfile(os.path.join(cpython_path, "Include", "internal", "pystate.h")):
        if os.path.isfile(os.path.join(cpython_path, "Include", "internal", "pycore_pystate.h")):
            program = program.replace("pystate.h", "pycore_pystate.h")
        else:
            print("failed to find Include/internal/pystate.h in cpython directory =(")
            return

    with tempfile.TemporaryDirectory() as path:
        if sys.platform.startswith("win"):
            source_filename = os.path.join(path, "pyruntime_offsets.cpp")
            exe = os.path.join("pyruntime_offsets.exe")
        else:
            source_filename = os.path.join(path, "pyruntime_offsets.c")
            exe = os.path.join(path, "pyruntime_offsets")

        with open(source_filename, "w") as o:
            o.write(program)
        if sys.platform.startswith("win"):
            # this requires a 'x64 Native Tools Command Prompt' to work out properly for 64 bit installs
            # also expects that you have run something like 'PCBuild\build.bat' first
            ret = os.system(f"cl {source_filename} /I {cpython_path} /I {cpython_path}\PC /I {cpython_path}\Include")
        elif sys.platform.startswith("freebsd"):
            ret = os.system(f"""cc {source_filename} -I {cpython_path} -I {cpython_path}/Include -o {exe}""")
        else:
            ret = os.system(f"""gcc {source_filename} -I {cpython_path} -I {cpython_path}/Include -o {exe}""")
        if ret:
            print("Failed to compile""")
            return ret

        ret = os.system(exe)
        if ret:
            print("Failed to run pyruntime file")
            return ret


def extract_bindings(cpython_path, version, configure=False):
    print("Generating bindings for python %s from repo at %s" % (version, cpython_path))

    ret = os.system(f"""
        cd {cpython_path}
        git checkout {version}

        # need to run configure on the current branch to generate pyconfig.h sometimes
        {("./configure prefix=" + os.path.abspath(os.path.join(cpython_path, version))) if configure else ""}

        cat Include/Python.h > bindgen_input.h
        cat Include/frameobject.h >> bindgen_input.h
        cat Objects/dict-common.h >> bindgen_input.h
        echo '#define Py_BUILD_CORE 1\n' >> bindgen_input.h
        cat Include/internal/pycore_pystate.h >> bindgen_input.h
        cat Include/internal/pycore_interp.h >> bindgen_input.h

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
            --whitelist-type PyTupleObject \
            --whitelist-type PyListObject \
            --whitelist-type PyIntObject \
            --whitelist-type PyLongObject \
            --whitelist-type PyFloatObject \
            --whitelist-type PyDictObject \
            --whitelist-type PyDictKeysObject \
            --whitelist-type PyDictKeyEntry \
            --whitelist-type PyObject \
            --whitelist-type PyTypeObject \
             -- -I . -I ./Include -I ./Include/internal
    """)
    if ret:
        return ret

    # write the file out to the appropriate place, disabling some warnings
    with open(os.path.join("src", "python_bindings", version.replace(".", "_") + ".rs"), "w") as o:
        o.write(f"// Generated bindings for python {version}\n")
        o.write("#![allow(dead_code)]\n")
        o.write("#![allow(non_upper_case_globals)]\n")
        o.write("#![allow(non_camel_case_types)]\n")
        o.write("#![allow(non_snake_case)]\n")
        o.write("#![allow(clippy::useless_transmute)]\n")
        o.write("#![allow(clippy::default_trait_access)]\n")
        o.write("#![allow(clippy::cast_lossless)]\n")
        o.write("#![allow(clippy::trivially_copy_pass_by_ref)]\n\n")
        o.write(open(os.path.join(cpython_path, "bindgen_output.rs")).read())


if __name__ == "__main__":

    if sys.platform.startswith("win"):
        default_cpython_path = os.path.join(os.getenv("userprofile"), "code", "cpython")
    else:
        default_cpython_path = os.path.join(os.getenv("HOME"), "code", "cpython")

    parser = argparse.ArgumentParser(description="runs bindgen on cpython version",
                                     formatter_class=argparse.ArgumentDefaultsHelpFormatter)
    parser.add_argument("--cpython", type=str, default=default_cpython_path,
                        dest="cpython", help="path to cpython repo")
    parser.add_argument("--configure",
                        help="Run configure script prior to generating bindings",
                        action="store_true")
    parser.add_argument("--pyruntime",
                        help="generate offsets for pyruntime",
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
        versions = ['v3.8.0b4', 'v3.7.0', 'v3.6.6', 'v3.5.5', 'v3.4.8', 'v3.3.7', 'v3.2.6', 'v2.7.15']
    else:
        versions = args.versions
        if not versions:
            print("You must specify versions of cpython to generate bindings for, or --all\n")
            parser.print_help()

    for version in versions:
        if args.build:
            # todo: this probably should be a separate script
            if build_python(args.cpython, version):
                print("Failed to build python")
        elif args.pyruntime:
            calculate_pyruntime_offsets(args.cpython, version, configure=args.configure)

        else:
            if extract_bindings(args.cpython, version, configure=args.configure):
                print("Failed to generate bindings")
