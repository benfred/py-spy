import os
import shutil
import sys

from setuptools import setup
from setuptools.command.install import install

from setuptools_rust import Binding, RustExtension

try:
    import pypandoc
    long_description = pypandoc.convert_file("README.md", "rst")
except ImportError:
    long_description = ''

executable_name = "py-spy.exe" if sys.platform.startswith("win") else "py-spy"

class PostInstallCommand(install):
    """Post-installation for installation mode."""
    def run(self):
        # So ths builds the executable, and even installs it
        # but we can't install to the bin directory:
        #     https://github.com/pypa/setuptools/issues/210#issuecomment-216657975
        # take the advice from that comment, and move over after install
        install.run(self)

        # we're going to install the py-spy executable into the scripts directory
        # but first make sure the scripts directory exists
        if not os.path.isdir(self.install_scripts):
            os.makedirs(self.install_scripts)

        # copy the binary over
        source_dir = os.path.dirname(os.path.abspath(__file__))
        source = os.path.join(source_dir, "target", "release", executable_name)
        target = os.path.join(self.install_scripts, executable_name)
        self.move_file(source, target)


setup(name='py-spy',
      author="Ben Frederickson",
      author_email="ben@benfrederickson.com",
      url='https://github.com/benfred/py-spy',
      description="A Sampling Profiler for Python",
      long_description=long_description,
      version="0.1.1",
      rust_extensions=[RustExtension('py_spy/py-spy', 'Cargo.toml', binding=Binding.Exec)],
      license="GPL",
      cmdclass={'install': PostInstallCommand},
      classifiers=[
        "Development Status :: 3 - Alpha",
        "Programming Language :: Python :: 3",
        "Programming Language :: Python :: 2",
        "Intended Audience :: Developers",
        "License :: OSI Approved :: GNU General Public License v3 (GPLv3)",
        "Topic :: Software Development :: Libraries",
        "Topic :: Utilities"],
      zip_safe=False)
