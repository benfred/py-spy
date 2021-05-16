import os
import sys
import platform

from setuptools import setup
from setuptools.command.install import install


# from https://stackoverflow.com/questions/45150304/how-to-force-a-python-wheel-to-be-platform-specific-when-building-it # noqa
try:
    from wheel.bdist_wheel import bdist_wheel as _bdist_wheel

    class bdist_wheel(_bdist_wheel):
        def finalize_options(self):
            _bdist_wheel.finalize_options(self)
            # Mark us as not a pure python package (we have platform specific rust code)
            self.root_is_pure = False

        def get_tag(self):
            # this set's us up to build generic wheels, since we can handle multiple
            # versions of python and have no ABI requirements
            python, abi, plat = _bdist_wheel.get_tag(self)
            cross_compile_target = _get_cross_compile_target()
            if cross_compile_target == "armv7-unknown-linux-musleabihf":
                plat = "manylinux2014_armv7l"
            elif cross_compile_target == "aarch64-unknown-linux-musl":
                plat = "manylinux2014_aarch64"
            elif cross_compile_target == "i686-unknown-linux-musl":
                plat = "manylinux1_i686"
            elif cross_compile_target == "x86_64-unknown-linux-musl":
                plat = "manylinux1_x86_64"
            elif platform.system() == "Darwin" and os.getenv('MACOSX_DEPLOYMENT_TARGET'):
                target = os.environ['MACOSX_DEPLOYMENT_TARGET']
                plat = "macosx_{}_{}".format(target.replace(".", "_"), platform.machine())

            python, abi = "py2.py3", "none"
            return python, abi, plat


except ImportError:
    bdist_wheel = None


with open("README.md") as f:
    long_description = f.read()

executable_name = "py-spy.exe" if sys.platform.startswith("win") else "py-spy"


class PostInstallCommand(install):
    """Post-installation for installation mode."""

    def run(self):
        # So ths builds the executable, and even installs it
        # but we can't install to the bin directory:
        #     https://github.com/pypa/setuptools/issues/210#issuecomment-216657975
        # take the advice from that comment, and move over after install
        source_dir = os.path.dirname(os.path.abspath(__file__))

        # if we have these env variables defined, then compile against the musl toolchain
        # this lets us statically link in libc (rather than have a glibc that might cause
        # issues like https://github.com/benfred/py-spy/issues/5.
        # Note: we're only doing this on demand since this requires musl-tools installed
        # but the released wheels should have this option set
        cross_compile_target = _get_cross_compile_target()
        if cross_compile_target:
            compile_args = " --target=%s" % cross_compile_target
            build_dir = os.path.join(source_dir, "target", cross_compile_target, "release")
        else:
            compile_args = ""
            build_dir = os.path.join(source_dir, "target", "release")

        if platform.system() == "Darwin":
            os.environ.setdefault("MACOSX_DEPLOYMENT_TARGET", "10.9")

        # setuptools_rust doesn't seem to let me specify a musl cross compilation target
        # so instead just build ourselves here =(.
        if os.system("cargo build --release %s" % compile_args):
            raise ValueError("Failed to compile!")

        # run this after trying to build with cargo (as otherwise this leaves
        # venv in a bad state: https://github.com/benfred/py-spy/issues/69)
        install.run(self)

        # we're going to install the py-spy executable into the scripts directory
        # but first make sure the scripts directory exists
        if not os.path.isdir(self.install_scripts):
            os.makedirs(self.install_scripts)

        source = os.path.join(build_dir, executable_name)
        target = os.path.join(self.install_scripts, executable_name)
        if os.path.isfile(target):
            os.remove(target)

        self.copy_file(source, target)


def _get_cross_compile_target():
    return os.getenv("PYSPY_CROSS_COMPILE_TARGET") or os.getenv("RUST_MUSL_CROSS_TARGET")


setup(
    name="py-spy",
    author="Ben Frederickson",
    author_email="ben@benfrederickson.com",
    url="https://github.com/benfred/py-spy",
    description="A Sampling Profiler for Python",
    long_description=long_description,
    long_description_content_type="text/markdown",
    version="0.3.7",
    license="MIT",
    cmdclass={"install": PostInstallCommand, "bdist_wheel": bdist_wheel},
    classifiers=[
        "Development Status :: 5 - Production/Stable",
        "Programming Language :: Python :: 3",
        "Programming Language :: Python :: 2",
        "Intended Audience :: Developers",
        "License :: OSI Approved :: MIT License",
        "Topic :: Software Development :: Libraries",
        "Topic :: Utilities",
    ],
    zip_safe=False,
)
