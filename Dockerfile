FROM python:3.12-slim

ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1

RUN apt-get update \
    && apt-get install -y --no-install-recommends git \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /opt/review-gate
COPY pyproject.toml README.md ./
COPY src ./src
RUN pip install --no-cache-dir .

# Run against a mounted repository.
WORKDIR /src

ENTRYPOINT ["review-gate"]
CMD ["--help"]
