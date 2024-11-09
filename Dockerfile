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

ENV KALATORI_HOST="0.0.0.0:16726"
ENV KALATORI_SEED="bottom drive obey lake curtain smoke basket hold race lonely fit walk"
ENV KALATORI_RECIPIENT="5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY"
ENV KALATORI_REMARK="test"

CMD ["/app/kalatori"]
