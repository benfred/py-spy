## v0.2.0 (pre-release)

* Add ability to profile native python extensions [#2](https://github.com/benfred/py-spy/issues/2)
* Add FreeBSD support [#112](https://github.com/benfred/py-spy/issues/112)
* Add option to write out Speedscope files [#115](https://github.com/benfred/py-spy/issues/115)
* Add option to output raw call stack data [#35](https://github.com/benfred/py-spy/issues/35)
* Get thread idle status from OS [#92](https://github.com/benfred/py-spy/issues/92)
* Add 'unlimited' default option for the duration [#93](https://github.com/benfred/py-spy/issues/93)
* Allow use as a library by other rust programs [#110](https://github.com/benfred/py-spy/issues/110)
* Show OS threadids in dump [#57](https://github.com/benfred/py-spy/issues/57)
* Drop root permissions when starting new process [#116](https://github.com/benfred/py-spy/issues/116)
* Support building for ARM processors [#89](https://github.com/benfred/py-spy/issues/89)
* Python 3.8 compatability

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
