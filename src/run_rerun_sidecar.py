"""Run Rerun beside an existing navigation service without starting navigation."""

import argparse
import asyncio
import logging
from pathlib import Path
from threading import Event, Thread

import uvicorn

from visualization.replay_store import ReplayStore
from visualization.rerun_sidecar import (
    LatestImageFetcher,
    ObservedNavigation,
    UpstreamClient,
    WebVizConsumer,
    create_sidecar_app,
)
from visualization.rerun_sink import _RerunRuntime


def parse_args(argv=None):
    repo_root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(
        description="Observe an existing WebViz service and publish it through Rerun.",
    )
    parser.add_argument(
        "--upstream",
        required=True,
        help="Existing robot WebViz base URL, for example http://192.168.123.161:8008.",
    )
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--control-port", type=int, default=8010)
    parser.add_argument("--rerun-port", type=int, default=9876)
    parser.add_argument("--server-memory-limit", default="32MiB")
    parser.add_argument("--queue-size", type=int, default=256)
    parser.add_argument("--map-poll-interval", type=float, default=5.0)
    parser.add_argument(
        "--history-dir",
        type=Path,
        default=repo_root / "log" / "rerun_sidecar_history",
    )
    parser.add_argument("--max-replay-mb", type=int, default=512)
    parser.add_argument(
        "--cors-allow-origin",
        action="append",
        default=[],
        help="Additional origin accepted by the Rerun gRPC server; repeat as needed.",
    )
    return parser.parse_args(argv)


def main(argv=None):
    args = parse_args(argv)
    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    replay_store = ReplayStore(
        history_dir=args.history_dir,
        max_replay_mb=args.max_replay_mb,
    )
    # The remote sidecar already runs in the NumPy 2/Rerun environment, so use
    # the runtime directly and avoid the robot-only isolation process plus IPC.
    rerun_sink = _RerunRuntime(
        enabled=True,
        grpc_port=args.rerun_port,
        server_memory_limit=args.server_memory_limit,
        cors_allow_origin=args.cors_allow_origin,
        save_rrd=True,
        history_dir=args.history_dir,
        queue_size=args.queue_size,
    )
    upstream = UpstreamClient(args.upstream)
    observer = ObservedNavigation(
        rerun_sink=rerun_sink,
        replay_store=replay_store,
    )
    image_fetcher = LatestImageFetcher(upstream, observer.publish_image)
    observer.set_image_scheduler(image_fetcher)
    consumer = WebVizConsumer(
        upstream,
        observer,
        map_poll_interval=args.map_poll_interval,
    )
    stop_event = Event()
    consumer_thread = Thread(
        target=lambda: asyncio.run(consumer.run(stop_event)),
        name="rerun-sidecar-webviz",
        daemon=True,
    )
    app = create_sidecar_app(observer=observer, upstream=upstream)

    image_fetcher.start()
    consumer_thread.start()
    try:
        uvicorn.run(
            app,
            host=args.host,
            port=args.control_port,
            log_level="info",
            access_log=False,
        )
    finally:
        stop_event.set()
        consumer_thread.join(timeout=3.0)
        image_fetcher.shutdown(timeout=2.0)
        observer.shutdown(timeout=5.0)


if __name__ == "__main__":
    main()
