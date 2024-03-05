FROM ubuntu:latest
RUN mkdir /app
COPY target/release/kalatori /app/kalatori

ENV KALATORI_HOST="0.0.0.0:16726"
ENV KALATORI_RPC="wss://rpc.ibp.network/polkadot"

WORKDIR /app
EXPOSE 16726

CMD ["./kalatori"]
