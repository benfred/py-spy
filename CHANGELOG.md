# Release notes are now being hosted in Github Releases: https://github.com/benfred/py-spy/releases

## v0.3.11

* Update dependencies [#463](https://github.com/benfred/py-spy/pull/463), [#457](https://github.com/benfred/py-spy/pull/463)
* Warn about SYS_PTRACE when running in docker [#459](https://github.com/benfred/py-spy/pull/459)
* Fix spelling mistakes [#453](https://github.com/benfred/py-spy/pull/453)

## v0.3.10
* Add support for profiling Python v3.10 [#425](https://github.com/benfred/py-spy/pull/425)
* Fix issue with native profiling on Linux with Anaconda [#447](https://github.com/benfred/py-spy/pull/447)

## v0.3.9
* Add a subcommand to generate shell completions [#427](https://github.com/benfred/py-spy/issues/427)
* Allow attaching co_firstlineno to frame name [#428](https://github.com/benfred/py-spy/issues/428)
* Fix speedscope time interval [#434](https://github.com/benfred/py-spy/issues/434)
* Fix profiling on FreeBSD [#431](https://github.com/benfred/py-spy/issues/431)
* Use GitHub actions for FreeBSD CI [#433](https://github.com/benfred/py-spy/issues/433)

## v0.3.8
* Add wheels for Apple Silicon [#419](https://github.com/benfred/py-spy/issues/419)
* Add --gil and --idle options to top view [#406](https://github.com/benfred/py-spy/issues/406)
* Fix errors parsing python binaries [#407](https://github.com/benfred/py-spy/issues/407)
* Specify timeunit in speedscope profiles [#294](https://github.com/benfred/py-spy/issues/294)

## v0.3.7
* Fix error that sometimes left the profiled program suspended [#390](https://github.com/benfred/py-spy/issues/390)
* Documentation fixes for README [#391](https://github.com/benfred/py-spy/issues/391), [#393](https://github.com/benfred/py-spy/issues/393)

## v0.3.6
* Fix profiling inside a venv on windows [#216](https://github.com/benfred/py-spy/issues/216)
* Detect GIL on Python 3.9.3+, 3.8.9+ [#375](https://github.com/benfred/py-spy/issues/375)
* Fix getting thread names on python 3.9 [#387](https://github.com/benfred/py-spy/issues/387)
* Fix getting thread names on ARMv7 [#388](https://github.com/benfred/py-spy/issues/388)
* Add python integration tests, and test wheels across a range of different python versions [#378](https://github.com/benfred/py-spy/pull/378)
* Automatically add tests for new versions of python [#379](https://github.com/benfred/py-spy/pull/379)

## v0.3.5
* Handle case where linux kernel is compiled without ```process_vm_readv``` support  [#22](https://github.com/benfred/py-spy/issues/22)
* Handle case where /proc/self/ns/mnt is missing [#326](https://github.com/benfred/py-spy/issues/326)
* Allow attaching to processes where the python binary has been deleted [#109](https://github.com/benfred/py-spy/issues/109)
* Make '--output' optional [#229](https://github.com/benfred/py-spy/issues/229)
* Add --full-filenames to allow showing full Python filenames [#363](https://github.com/benfred/py-spy/issues/363)
* Count "samples" as the number of recorded stacks (per thread) [#365](https://github.com/benfred/py-spy/issues/365)
* Exit with an error if --gil but we failed to get necessary addrs/offsets [#361](https://github.com/benfred/py-spy/pull/361)
* Include command/options used to run py-spy in flamegraph output [#293](https://github.com/benfred/py-spy/issues/293)
* GIL Detection fixes for python 3.9.2/3.8.8 [#362](https://github.com/benfred/py-spy/pull/362)
* Move to Github Actions for CI

## v0.3.4
* Build armv7/aarch64 wheels [#328](https://github.com/benfred/py-spy/issues/328)
* Detect GIL on Python 3.9 / 3.7.7+ / 3.8.2+
* Add option for more verbose local variables [#287](https://github.com/benfred/py-spy/issues/287)
* Fix issues with profiling subprocesses [#265](https://github.com/benfred/py-spy/issues/265)
* Include python thread names in record [#237](https://github.com/benfred/py-spy/issues/237)
* Fix issue with threadids triggering differential flamegraphs [#234](https://github.com/benfred/py-spy/issues/234)

## v0.3.3

* Change to display stdout/stderr from profiled child process [#217](https://github.com/benfred/py-spy/issues/217)
* Fix memory leak on OSX [#227](https://github.com/benfred/py-spy/issues/227)
* Fix panic on dump --locals [#224](https://github.com/benfred/py-spy/issues/224)
* Fix cross container short filename generation [#220](https://github.com/benfred/py-spy/issues/220)

## v0.3.2

* Fix line numbers on python 3.8+ [#190](https://github.com/benfred/py-spy/issues/190)
* Fix profiling pyinstaller binaries on OSX [#207](https://github.com/benfred/py-spy/issues/207)
* Support getting GIL from Python 3.8.1/3.7.6/3.7.5 [#211](https://github.com/benfred/py-spy/issues/211)

## v0.3.1

* Fix ptrace errors on linux kernel older than v4.7 [#83](https://github.com/benfred/py-spy/issues/83)
* Fix for profiling docker containers from host os [#199](https://github.com/benfred/py-spy/issues/199)
* Fix for speedscope profiles aggregated by function name [#201](https://github.com/benfred/py-spy/issues/201)
* Use symbols from dynsym table of ELF binaries [#191](https://github.com/benfred/py-spy/pull/191)

## v0.3.0

* Add ability to profile subprocesses [#124](https://github.com/benfred/py-spy/issues/124)
* Fix overflow issue with linux symbolication [#183](https://github.com/benfred/py-spy/issues/183)
* Fixes for printing local variables [#180](https://github.com/benfred/py-spy/pull/180)

## v0.2.2

* Add ability to show local variables when dumping out stack traces [#77](https://github.com/benfred/py-spy/issues/77)
* Show python thread names in dump [#47](https://github.com/benfred/py-spy/issues/47)
* Fix issues with profiling python hosted by .NET exe [#171](https://github.com/benfred/py-spy/issues/171)

## v0.2.1

* Fix issue with profiling dockerized process from the host os [#168](https://github.com/benfred/py-spy/issues/168)

## v0.2.0

* Add ability to profile native python extensions [#2](https://github.com/benfred/py-spy/issues/2)
* Add FreeBSD support [#112](https://github.com/benfred/py-spy/issues/112)
* Relicense to MIT [#163](https://github.com/benfred/py-spy/issues/163)
* Add option to write out Speedscope files [#115](https://github.com/benfred/py-spy/issues/115)
* Add option to output raw call stack data [#35](https://github.com/benfred/py-spy/issues/35)
* Get thread idle status from OS [#92](https://github.com/benfred/py-spy/issues/92)
* Add 'unlimited' default option for the duration [#93](https://github.com/benfred/py-spy/issues/93)
* Allow use as a library by other rust programs [#110](https://github.com/benfred/py-spy/issues/110)
* Show OS threadids in dump [#57](https://github.com/benfred/py-spy/issues/57)
* Drop root permissions when starting new process [#116](https://github.com/benfred/py-spy/issues/116)
* Support building for ARM processors [#89](https://github.com/benfred/py-spy/issues/89)
* Python 3.8 compatibility
* Fix issues profiling functions with more than 4000 lines [#164](https://github.com/benfred/py-spy/issues/164)

## v0.1.11

* Fix to detect GIL status on Python 3.7+ [#104](https://github.com/benfred/py-spy/pull/104)
* Generate flamegraphs without perl (using Inferno) [#38](https://github.com/benfred/py-spy/issues/38)
* Use irregular sampling interval to avoid incorrect results [#94](https://github.com/benfred/py-spy/issues/94)
* Detect python packages when generating short filenames [#75](https://github.com/benfred/py-spy/issues/75)
* Fix issue with finding interpreter with Python 3.7 and 32bit Linux [#101](https://github.com/benfred/py-spy/issues/101)
* Detect "v2.7.15+" as a valid version string [#81](https://github.com/benfred/py-spy/issues/81)
* Fix to cleanup venv after failing to build with setup.py [#69](https://github.com/benfred/py-spy/issues/69)

## v0.1.10

* Fix running py-spy inside a docker container [#68](https://github.com/benfred/py-spy/issues/68)

## v0.1.9

* Fix partial stack traces from showing up, by pausing process while collecting samples [#56](https://github.com/benfred/py-spy/issues/56). Also add a ```--nonblocking``` option to use previous behaviour of not stopping process.
* Allow sampling process running in a docker container from the host OS [#49](https://github.com/benfred/py-spy/issues/49)
* Allow collecting data for flame graph until interrupted with Control-C  [#21](https://github.com/benfred/py-spy/issues/21)
* Support 'legacy' strings in python 3 [#64](https://github.com/benfred/py-spy/issues/64)

## v0.1.8

* Support profiling pyinstaller binaries [#42](https://github.com/benfred/py-spy/issues/42)
* Add fallback when failing to find exe in memory maps [#40](https://github.com/benfred/py-spy/issues/40)

## v0.1.7

* Console viewer improvements for Windows 7 [#37](https://github.com/benfred/py-spy/issues/37)

## v0.1.6

* Warn if we can't sample fast enough [#33](https://github.com/benfred/py-spy/issues/33)
* Support embedded python interpreters like UWSGI [#25](https://github.com/benfred/py-spy/issues/25)
* Better error message when failing with 32-bit python on windows

## v0.1.5

* Use musl libc for linux wheels [#5](https://github.com/benfred/py-spy/issues/5)
* Fix for OSX python built with '--enable-framework' [#15](https://github.com/benfred/py-spy/issues/15)
* Fix for running on Centos7

## v0.1.4

* Initial public release
