FROM rust:1.56.1 as builder
WORKDIR /usr/src/myapp
COPY . .
RUN cargo install --path .

FROM debian:buster-slim
RUN apt-get update && \
    apt-get install -y libssl1.1 ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/cargo/bin/mikes_crawler /usr/local/bin/mikes_crawler
ENV ROCKET_ENV=production ROCKET_ADDRESS=0.0.0.0
CMD ["mikes_crawler"]
EXPOSE 8000