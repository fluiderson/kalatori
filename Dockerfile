FROM rust:1.82 as builder

WORKDIR /usr/src/kalatori

COPY Cargo.toml Cargo.lock ./

RUN mkdir -p src && echo "fn main() {}" > src/main.rs

RUN cargo build --release

RUN rm -rf src
COPY . .

RUN cargo build --release

FROM ubuntu:latest

WORKDIR /app

COPY --from=builder /usr/src/kalatori/target/release/kalatori /app/kalatori

EXPOSE 16726

CMD ["/app/kalatori"]
