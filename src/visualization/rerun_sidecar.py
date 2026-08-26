"""Remote-side adapter from an existing robot WebViz backend to Rerun.

The sidecar deliberately does not import ROS or any navigation nodes. It consumes
the robot's already-running navigation WebSocket, performs Rerun conversion and
recording on the operator computer, and exposes the compact local HTTP API
expected by the native operator.
"""

import asyncio
from copy import deepcopy
import json
import logging
from pathlib import Path
from threading import Condition, Event, Lock, Thread
from time import monotonic, time
from urllib.parse import urljoin, urlparse

import requests
from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse, JSONResponse

from .replay_store import ReplayStore
from .state_hub import VisualizationStateHub


logger = logging.getLogger(__name__)

NO_CACHE_HEADERS = {"Cache-Control": "no-store, no-cache, must-revalidate, max-age=0"}
ACTIVE_TASK_STATUSES = {"processing", "busy", "running"}
TERMINAL_TASK_STATUSES = {"completed", "failed", "stopped", "cancelled", "canceled"}
TERMINAL_EVENTS = {
    "task_completed": "completed",
    "task_failed": "failed",
    "task_stopped": "stopped",
    "task_cancelled": "cancelled",
    "task_canceled": "canceled",
}


class UpstreamError(RuntimeError):
    """An existing navigation HTTP endpoint could not be read or proxied."""


class UpstreamClient:
    """Small HTTP client scoped to one existing navigation backend."""

    def __init__(self, base_url="http://127.0.0.1:8008", timeout=(1.0, 5.0), session=None):
        self.base_url = str(base_url).rstrip("/")
        self.timeout = timeout
        self._session = session or requests.Session()
        if session is None:
            # Robot LAN traffic must not be routed through a workstation proxy.
            self._session.trust_env = False

    @property
    def websocket_url(self):
        parsed = urlparse(self.base_url)
        scheme = "wss" if parsed.scheme == "https" else "ws"
        return parsed._replace(scheme=scheme, path="/viz/ws", params="", query="", fragment="").geturl()

    def _url(self, path):
        return urljoin(f"{self.base_url}/", str(path).lstrip("/"))

    def get_json(self, path):
        return self._request_json("GET", path)

    def post_json(self, path, payload):
        return self._request_json("POST", path, payload=payload)

    def _request_json(self, method, path, payload=None):
        try:
            response = self._session.request(
                method,
                self._url(path),
                json=payload,
                timeout=self.timeout,
            )
            response.raise_for_status()
            value = response.json()
        except (requests.RequestException, ValueError) as exc:
            raise UpstreamError(f"{method} {path} failed: {exc}") from exc
        if not isinstance(value, (dict, list)):
            raise UpstreamError(f"{method} {path} returned an unsupported JSON value")
        return value

    def get_image(self, url_or_path):
        try:
            response = self._session.get(self._url(url_or_path), timeout=self.timeout)
            response.raise_for_status()
        except requests.RequestException as exc:
            raise UpstreamError(f"GET {url_or_path} failed: {exc}") from exc
        return response.content, response.headers.get("content-type", "image/jpeg").split(";", 1)[0]


class LatestImageFetcher:
    """Fetch only the newest pending version of each WebViz image stream."""

    def __init__(self, upstream, publish_image):
        self._upstream = upstream
        self._publish_image = publish_image
        self._condition = Condition()
        self._pending = {}
        self._published = {}
        self._stopping = False
        self._thread = None

    def start(self):
        with self._condition:
            if self._thread is not None and self._thread.is_alive():
                return
            self._stopping = False
            self._thread = Thread(target=self._run, name="rerun-sidecar-images", daemon=True)
            self._thread.start()

    def schedule(self, key, descriptor):
        if not isinstance(descriptor, dict) or not descriptor.get("url"):
            return False
        version = descriptor.get("version")
        identity = (version, str(descriptor["url"]))
        with self._condition:
            if self._published.get(str(key)) == identity:
                return False
            self._pending[str(key)] = {
                "url": str(descriptor["url"]),
                "version": version,
                "updated_at": descriptor.get("updated_at"),
                "identity": identity,
            }
            self._condition.notify()
        return True

    def shutdown(self, timeout=2.0):
        with self._condition:
            self._stopping = True
            self._condition.notify_all()
            thread = self._thread
        if thread is not None:
            thread.join(timeout=max(0.0, float(timeout)))
        return thread is None or not thread.is_alive()

    def _run(self):
        while True:
            with self._condition:
                while not self._pending and not self._stopping:
                    self._condition.wait()
                if self._stopping:
                    return
                key = next(iter(self._pending))
                descriptor = self._pending.pop(key)
            try:
                content, content_type = self._upstream.get_image(descriptor["url"])
                self._publish_image(
                    key,
                    descriptor.get("version"),
                    descriptor.get("updated_at"),
                    content,
                    content_type,
                )
                with self._condition:
                    self._published[key] = descriptor["identity"]
            except Exception:
                logger.exception("Failed to mirror WebViz image %s into Rerun", key)


class ObservedNavigation:
    """Translate an existing WebViz stream without creating navigation nodes."""

    def __init__(self, *, rerun_sink, replay_store, image_scheduler=None):
        self.rerun_sink = rerun_sink
        self.replay_store = replay_store
        self.state_hub = VisualizationStateHub(
            replay_store=None,
            save_replay=False,
            rerun_sink=None,
        )
        self._image_scheduler = image_scheduler
        self._connection_lock = Lock()
        self._upstream_connected = False
        self._upstream_error = None
        self._last_message_at = None
        self._map_metadata = {}

    def set_image_scheduler(self, scheduler):
        self._image_scheduler = scheduler

    def set_connection(self, connected, error=None):
        with self._connection_lock:
            self._upstream_connected = bool(connected)
            self._upstream_error = None if connected else (str(error) if error else None)

    def update_map(self, metadata, image_content=None, image_content_type="image/png"):
        if not isinstance(metadata, dict):
            return False
        with self._connection_lock:
            if metadata == self._map_metadata and image_content is None:
                return False
            self._map_metadata = deepcopy(metadata)
        rerun_metadata = deepcopy(metadata)
        if image_content is not None:
            rerun_metadata["_rerun_image_bytes"] = bytes(image_content)
            rerun_metadata["_rerun_image_media_type"] = str(image_content_type)
        self.rerun_sink.update_map(rerun_metadata)
        return True

    def map_metadata(self):
        with self._connection_lock:
            return deepcopy(self._map_metadata)

    def status(self):
        rerun_status = dict(self.rerun_sink.status())
        with self._connection_lock:
            rerun_status.update(
                {
                    "upstream_connected": self._upstream_connected,
                    "upstream_error": self._upstream_error,
                    "last_upstream_message_at": self._last_message_at,
                }
            )
        return rerun_status

    def operator_status(self):
        status = self.state_hub.build_operator_status()
        with self._connection_lock:
            status["upstream_connected"] = self._upstream_connected
            status["upstream_error"] = self._upstream_error
            status["last_upstream_message_at"] = self._last_message_at
        return status

    def handle_message(self, message):
        if not isinstance(message, dict):
            return
        with self._connection_lock:
            self._last_message_at = time()
        message_type = message.get("type")
        if message_type == "hello":
            return
        if message_type == "snapshot":
            self._handle_snapshot(message)
        elif message_type == "task_status":
            self._handle_task_status(message)
        elif message_type == "goal_update":
            self._handle_goal(message)
        elif message_type == "pose_update":
            self._handle_pose(message)
        elif message_type == "planner_update":
            self._handle_planner(message)
        elif message_type == "image_update":
            self._handle_images(message)
        elif message_type == "dynamic_map_update":
            self._handle_dynamic_map(message)
        elif message_type == "dynamic_map_recording_update":
            self.state_hub.set_dynamic_map_recording(bool(message.get("enabled")))
        elif message_type == "event":
            self._handle_event(message)
        else:
            self.rerun_sink.publish(message)

    def publish_image(self, key, version, updated_at, content, content_type):
        self.rerun_sink.publish_image(
            key,
            content,
            content_type=content_type,
            timestamp=updated_at,
        )

    def set_dynamic_map_recording(self, enabled):
        return self.state_hub.set_dynamic_map_recording(bool(enabled))

    def shutdown(self, timeout=5.0):
        active_task_id = self.state_hub.get_active_task_id()
        if active_task_id is not None:
            self._finish_task(active_task_id, "sidecar_stopped")
        self.replay_store.wait_for_pending_finalizations(timeout=timeout)
        replay_stopped = self.replay_store.shutdown(timeout=min(1.0, timeout))
        rerun_stopped = self.rerun_sink.shutdown(timeout=timeout)
        return replay_stopped and rerun_stopped

    def _handle_snapshot(self, message):
        timestamp = message.get("timestamp", time())
        task = message.get("task") or {}
        self._handle_task_status(
            {
                "type": "task_status",
                "schema_version": message.get("schema_version", "v1"),
                "timestamp": timestamp,
                **task,
            }
        )
        self._handle_goal(
            {
                "type": "goal_update",
                "schema_version": message.get("schema_version", "v1"),
                "timestamp": timestamp,
                "goal": message.get("goal") or {},
            }
        )
        self._handle_pose(
            {
                "type": "pose_update",
                "schema_version": message.get("schema_version", "v1"),
                "timestamp": timestamp,
                **(message.get("robot") or {}),
            }
        )
        self._handle_planner(
            {
                "type": "planner_update",
                "schema_version": message.get("schema_version", "v1"),
                "timestamp": timestamp,
                **(message.get("planner") or {}),
            }
        )
        self.state_hub.set_dynamic_map_recording(bool(message.get("dynamic_map_recording")))
        dynamic_map = message.get("dynamic_map")
        if dynamic_map:
            self._handle_dynamic_map(
                {
                    "type": "dynamic_map_update",
                    "schema_version": message.get("schema_version", "v1"),
                    "timestamp": timestamp,
                    **dynamic_map,
                }
            )
        self._handle_images(
            {
                "type": "image_update",
                "schema_version": message.get("schema_version", "v1"),
                "timestamp": timestamp,
                "images": message.get("images") or {},
            }
        )
        for event in message.get("events") or []:
            self._handle_event(event)

    def _handle_task_status(self, message):
        task_id = message.get("task_id")
        status = str(message.get("status") or "idle").lower()
        if task_id and status in ACTIVE_TASK_STATUSES:
            self._ensure_task(task_id, message.get("goal_text"))

        fields = {
            key: message[key]
            for key in (
                "task_id",
                "status",
                "result_status",
                "goal_text",
                "success_flag",
                "message",
                "state",
                "dry_run",
            )
            if key in message
        }
        self.state_hub.update_task_status(**fields)
        self.rerun_sink.publish(message)

        terminal_status = message.get("result_status")
        if status in TERMINAL_TASK_STATUSES:
            terminal_status = status
        elif status in ACTIVE_TASK_STATUSES:
            terminal_status = None
        if task_id and str(terminal_status or "").lower() in TERMINAL_TASK_STATUSES:
            self._finish_task(task_id, str(terminal_status).lower())

    def _handle_goal(self, message):
        goal = message.get("goal") or {}
        pose = goal.get("pose")
        if pose:
            self.state_hub.update_goal(
                pose.get("x"),
                pose.get("y"),
                pose.get("theta"),
                source=goal.get("source"),
            )
        self.rerun_sink.publish(message)

    def _handle_pose(self, message):
        self.state_hub.update_pose(
            pose=message.get("pose"),
            vpr_pose=message.get("vpr_pose"),
        )
        self.rerun_sink.publish(message)

    def _handle_planner(self, message):
        kwargs = {
            key: message[key]
            for key in (
                "mode",
                "global_path",
                "waypoints",
                "local_path",
                "local_goal",
                "actions",
                "action_limit",
                "preview_actions",
                "camera_intrinsics",
                "camera_image_size",
            )
            if key in message
        }
        self.state_hub.update_planner(**kwargs)
        self.rerun_sink.publish(message)

    def _handle_images(self, message):
        images = message.get("images") or {}
        if not isinstance(images, dict):
            return
        self.state_hub.update_images(**images)
        if self._image_scheduler is not None:
            for key, descriptor in images.items():
                self._image_scheduler.schedule(key, descriptor)

    def _handle_dynamic_map(self, message):
        payload = {
            key: value
            for key, value in message.items()
            if key not in {"type", "schema_version", "timestamp"}
        }
        self.state_hub.update_dynamic_map(payload, timestamp=message.get("timestamp"))
        self.rerun_sink.publish(
            message,
            record=self.state_hub.get_dynamic_map_recording(),
        )

    def _handle_event(self, message):
        if not isinstance(message, dict):
            return
        event_name = message.get("event_name")
        task_id = message.get("task_id")
        if event_name == "task_received" and task_id:
            payload = message.get("payload") or {}
            self._ensure_task(task_id, payload.get("goal_text"))
        self.state_hub.append_event(
            event_name or "unknown",
            task_id=task_id,
            payload=message.get("payload"),
            level=message.get("level", "info"),
        )
        self.rerun_sink.publish(message)
        terminal_status = TERMINAL_EVENTS.get(event_name)
        if task_id and terminal_status:
            self._finish_task(task_id, terminal_status)

    def _ensure_task(self, task_id, goal_text=None):
        task_id = str(task_id)
        active_task_id = self.state_hub.get_active_task_id()
        if active_task_id == task_id:
            return
        if active_task_id is not None:
            self._finish_task(active_task_id, "superseded")
        task_dir = self.replay_store.start_task(task_id=task_id, goal_text=goal_text)
        self.state_hub.start_task_recording(task_id=task_id, goal_text=goal_text)
        recording_path = task_dir / "recording.rrd"
        self.rerun_sink.start_task(
            task_id=task_id,
            goal_text=goal_text,
            recording_path=recording_path,
        )

    def _finish_task(self, task_id, status):
        if self.state_hub.get_active_task_id() != task_id:
            return
        snapshot = self.state_hub.build_snapshot()
        self.rerun_sink.finish_task(task_id=task_id, status=status)
        self.state_hub.finalize_task_recording(
            task_id=task_id,
            status=status,
        )
        self.replay_store.finalize_task_async(
            task_id=task_id,
            status=status,
            snapshot=snapshot,
        )


class WebVizConsumer:
    """Reconnect an observer to the existing navigation WebSocket."""

    def __init__(self, upstream, observer, *, map_poll_interval=5.0):
        self.upstream = upstream
        self.observer = observer
        self.map_poll_interval = max(1.0, float(map_poll_interval))
        self._map_signature = None

    async def run(self, stop_event):
        import websockets

        retry_delay = 1.0
        while not stop_event.is_set():
            try:
                async with websockets.connect(
                    self.upstream.websocket_url,
                    open_timeout=3,
                    close_timeout=1,
                    ping_interval=10,
                    ping_timeout=10,
                    max_size=64 * 1024 * 1024,
                    max_queue=8,
                    proxy=None,
                ) as websocket:
                    self.observer.set_connection(True)
                    self._refresh_map()
                    retry_delay = 1.0
                    last_map_poll = monotonic()
                    while not stop_event.is_set():
                        raw = None
                        try:
                            raw = await asyncio.wait_for(websocket.recv(), timeout=1.0)
                        except asyncio.TimeoutError:
                            pass
                        if raw is not None:
                            message = json.loads(raw)
                            self.observer.handle_message(message)
                        if monotonic() - last_map_poll >= self.map_poll_interval:
                            self._refresh_map()
                            last_map_poll = monotonic()
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                self.observer.set_connection(False, exc)
                logger.warning("WebViz upstream unavailable: %s", exc)
                await self._wait_for_stop(stop_event, retry_delay)
                retry_delay = min(10.0, retry_delay * 2.0)
        self.observer.set_connection(False, "sidecar stopped")

    def _refresh_map(self):
        try:
            metadata = self.upstream.get_json("/viz/api/map/metadata")
            signature = json.dumps(metadata, sort_keys=True, ensure_ascii=False, default=str)
            if signature == self._map_signature:
                return
            image_content = None
            image_content_type = "image/png"
            if metadata.get("image_path"):
                try:
                    image_content, image_content_type = self.upstream.get_image(
                        "/viz/api/map/image"
                    )
                except Exception:
                    # Keep publishing geometric metadata, but retry the map image
                    # on the next poll instead of caching a partial result.
                    self.observer.update_map(metadata)
                    logger.exception("Failed to download map image from WebViz")
                    return
            self.observer.update_map(
                metadata,
                image_content=image_content,
                image_content_type=image_content_type,
            )
            self._map_signature = signature
        except Exception:
            logger.exception("Failed to refresh map metadata from WebViz")

    @staticmethod
    async def _wait_for_stop(stop_event, timeout):
        deadline = monotonic() + max(0.0, float(timeout))
        while not stop_event.is_set() and monotonic() < deadline:
            await asyncio.sleep(min(0.2, max(0.0, deadline - monotonic())))


def create_sidecar_app(*, observer, upstream):
    """Build an operator-compatible API without any ROS/navigation imports."""

    app = FastAPI()
    app.state.observer = observer
    app.state.upstream = upstream

    def upstream_call(method, path, payload=None):
        try:
            if method == "GET":
                return upstream.get_json(path)
            return upstream.post_json(path, payload or {})
        except UpstreamError as exc:
            raise HTTPException(status_code=502, detail=str(exc)) from exc

    @app.get("/")
    def root():
        return JSONResponse(
            {
                "service": "woosh-rerun-sidecar",
                "navigation_mode": "read-only-observer",
                "upstream": upstream.base_url,
            },
            headers=NO_CACHE_HEADERS,
        )

    @app.get("/viz/api/rerun")
    def rerun_status():
        return JSONResponse(observer.status(), headers=NO_CACHE_HEADERS)

    @app.get("/viz/api/operator/status")
    def operator_status():
        return JSONResponse(observer.operator_status(), headers=NO_CACHE_HEADERS)

    @app.get("/viz/api/map/metadata")
    def map_metadata():
        metadata = upstream_call("GET", "/viz/api/map/metadata")
        observer.update_map(metadata)
        return JSONResponse(metadata, headers=NO_CACHE_HEADERS)

    @app.get("/viz/api/performance")
    def performance():
        return JSONResponse(
            {
                "available": False,
                "health": "unavailable",
                "timestamp": time(),
                "processes": {"navigation": {}, "rerun": {}, "monitor": {}},
                "warnings": [
                    {
                        "level": "info",
                        "code": "sidecar_performance_unavailable",
                        "message": "Sidecar 模式未读取导航性能日志",
                    }
                ],
            },
            headers=NO_CACHE_HEADERS,
        )

    @app.get("/viz/api/dynamic-map/recording")
    def dynamic_map_recording():
        return JSONResponse(
            {"enabled": observer.state_hub.get_dynamic_map_recording()},
            headers=NO_CACHE_HEADERS,
        )

    @app.post("/viz/api/dynamic-map/recording")
    def set_dynamic_map_recording(payload: dict):
        result = upstream_call("POST", "/viz/api/dynamic-map/recording", payload)
        enabled = bool(result.get("enabled")) if isinstance(result, dict) else bool(payload.get("enabled"))
        observer.set_dynamic_map_recording(enabled)
        return JSONResponse({"enabled": enabled}, headers=NO_CACHE_HEADERS)

    @app.post("/viz/api/navigation/stop")
    def stop_navigation():
        return JSONResponse(
            upstream_call("POST", "/viz/api/navigation/stop", {}),
            headers=NO_CACHE_HEADERS,
        )

    @app.post("/viz/api/navigation/text")
    def submit_text_navigation(payload: dict):
        return JSONResponse(
            upstream_call("POST", "/viz/api/navigation/text", payload),
            headers=NO_CACHE_HEADERS,
        )

    @app.get("/viz/api/replay/tasks")
    def replay_tasks(limit: int = 50):
        return JSONResponse(
            observer.replay_store.list_tasks(limit=max(1, min(50, int(limit)))),
            headers=NO_CACHE_HEADERS,
        )

    @app.get("/viz/api/replay/tasks/{task_id}/recording.rrd")
    def replay_recording(task_id: str):
        recording_path = observer.replay_store.get_rerun_recording(task_id)
        if recording_path is None:
            raise HTTPException(status_code=404, detail="Rerun recording not found")
        return FileResponse(
            Path(recording_path),
            media_type="application/octet-stream",
            filename=f"{task_id}.rrd",
            headers=NO_CACHE_HEADERS,
        )

    return app
