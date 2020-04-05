""" Helper script to build wheels for releases for OSX and Linux.

Assumes that we are running on a OSX machine, Linux wheels are
created through virtual machines running vagrant

Wheels will be in the dist/ folder after running.
"""
import logging
import os
import shutil
import sys

log = logging.getLogger("build_wheels")


def make_wheel_filename_generic(wheel):
    """ Wheel filenames contain the python version and the python ABI version
    for the wheel. https://www.python.org/dev/peps/pep-0427/#file-name-convention
    Since we're distributing a rust binary this doesn't matter for us ... """
    name, version, python, abi, platform = wheel.split("-")

    # our binary handles multiple abi/versions of python
    python, abi = "py2.py3", "none"

    # hack, lets pretend to be manylinux1 so we can do a binary distribution
    if platform == "linux_x86_64.whl":
        platform = "manylinux1_x86_64.whl"
    elif platform == "linux_i686.whl":
        platform = "manylinux1_i686.whl"

    return "-".join((name, version, python, abi, platform))


def local_build_wheel():
    path = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    ret = os.system("""
            cd %s
            python3 setup.py bdist_wheel
    """ % path)
    print(ret)
    if ret:
        sys.exit(ret)


def vagrant_build_wheel(vagrantfile):
    import vagrant
    v = vagrant.Vagrant(vagrantfile, quiet_stdout=False, quiet_stderr=False)
    v.up()
    v.halt()


def build_wheels(docker_image="rust_python3", build_local=True, vagrantfiles=None, clean=True):
    path = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    dist = os.path.join(path, "dist")
    log.info("Generating wheels @'%s'", dist)

    # clean up old wheels
    if clean:
        for filename in os.listdir(dist):
            filename = os.path.join(dist, filename)
            if filename.endswith(".whl") and os.path.isfile(filename):
                log.info("Deleting previous wheel '%s'", filename)
                os.unlink(filename)

    # generate wheels for current system (hopefully OSX)
    if build_local:
        local_build_wheel()

    # generate wheels for linux
    for vagrantfile in vagrantfiles or []:
        vagrant_build_wheel(vagrantfile)

    # rename wheels to remove python version/abi tags
    for wheel in os.listdir(dist):
        filename = os.path.join(dist, wheel)
        if filename.endswith(".whl") and os.path.isfile(filename):
            newfilename = os.path.join(dist, make_wheel_filename_generic(wheel))

            log.info("Moving %s -> %s", filename, newfilename)
            shutil.move(filename, newfilename)


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO)
    # build_wheels()
    import argparse
    parser = argparse.ArgumentParser("Parse setup.py files")
    parser.add_argument('--localonly', dest='localonly', action='store_true')
    args = parser.parse_args()

    if args.localonly:
        local_build_wheel()
    else:
        #build_wheels(vagrantfiles=["./ubuntu32", "./ubuntu64"])
        build_wheels(vagrantfiles=["./ubuntu64"])
