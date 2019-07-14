mktempd() {
    echo $(mktemp -d 2>/dev/null || mktemp -d -t tmp)
}

mk_tarball() {
    local td=$(mktempd)
    local out_dir=$(pwd)
    cp target/$TARGET/release/py-spy $td

    pushd $td

    # release tarball will look like 'rust-everywhere-v1.2.3-x86_64-unknown-linux-gnu.tar.gz'
    tar czf $out_dir/py-spy-${TRAVIS_TAG}-${TARGET}.tar.gz *

    popd
    rm -r $td
}

mk_tarball
