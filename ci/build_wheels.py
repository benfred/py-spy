""" Helper script to build wheels for releases for OSX and Linux.

Assumes that we are running on a OSX machine, Linux wheels are
created through docker.

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


def docker_build_wheel(docker_image):
    import docker
    path = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    client = docker.from_env()
    container = client.containers.run(docker_image,
                                      volumes={path: {'bind': '/home/py-spy', 'mode': 'rw'}},
                                      remove=True, detach=True, tty=True)
    try:
        result = container.exec_run("python3 /home/py-spy/ci/build_wheels.py --localonly")
        if result.exit_code:
            raise RuntimeError(result.output.decode("utf8"))

        print(result.output)
    finally:
        container.stop()
        client.close()


def build_wheels(docker_image="rust_python3", build_local=False, clean=False):
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
    if docker_image:
        docker_build_wheel(docker_image)

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
        build_wheels()
