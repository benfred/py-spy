source $HOME/.cargo/env
cd /vagrant
cargo build --release
cargo test --release
