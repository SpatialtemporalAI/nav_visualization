"""Non-blocking bridge from the navigation visualization state to Rerun."""

from copy import deepcopy
from io import BytesIO
import json
import logging
import math
import os
from pathlib import Path
import pickle
import queue
import struct
import subprocess
import sys
from threading import Lock, Thread
from time import time
import uuid


logger = logging.getLogger(__name__)


class _RerunRuntime:
    """Publish live data and per-task recordings without blocking ROS callbacks.

    Rerun is an optional dependency.  Import, server, or logging failures disable
    this sink only; navigation and the legacy JSON API keep running.
    """

    VERSION = "0.36.1"

    def __init__(
        self,
        *,
        enabled=True,
        grpc_port=9876,
        server_memory_limit="256MiB",
        cors_allow_origin=None,
        save_rrd=True,
        history_dir="./log/rerun_history",
        queue_size=256,
    ):
        self.enabled = False
        self.live_uri = None
        self.error = None
        self._rr = None
        self._live = None
        self._task = None
        self._active_task_id = None
        self._active_task_path = None
        self._map_metadata = None
        self._map_refresh_at = {}
        self._latest_planner = None
        self._image_sizes = {}
        self._save_rrd = bool(save_rrd)
        self._history_dir = Path(history_dir)
        self._queue = queue.Queue(maxsize=max(8, int(queue_size)))
        self._worker = None
        self._state_lock = Lock()

        if not enabled:
            return

        try:
            import rerun as rr

            installed_version = getattr(rr, "__version__", None)
            if installed_version != self.VERSION:
                raise RuntimeError(
                    f"rerun-sdk version mismatch: expected {self.VERSION}, got {installed_version}"
                )
            self._rr = rr
            self._live = rr.RecordingStream(
                "woosh_robot_navigation",
                recording_id=uuid.uuid4(),
            )
            default_blueprint = self._build_default_blueprint()
            self.live_uri = self._live.serve_grpc(
                grpc_port=int(grpc_port),
                default_blueprint=default_blueprint,
                server_memory_limit=str(server_memory_limit),
                newest_first=True,
                cors_allow_origin=list(cors_allow_origin or []),
            )
            self.enabled = True
            self._worker = Thread(target=self._run, name="rerun-sink", daemon=True)
            self._worker.start()
        except Exception as exc:
            self.error = str(exc)
            logger.exception("Rerun visualization is unavailable; continuing without it")
            self._disconnect_stream(self._live)
            self._live = None

    def status(self):
        with self._state_lock:
            return {
                "enabled": self.enabled,
                "version": self.VERSION,
                "live_uri": self.live_uri,
                "error": self.error,
                "active_task_id": self._active_task_id,
            }

    @staticmethod
    def _build_default_blueprint():
        import rerun.blueprint as rrb

        return rrb.Blueprint(
            rrb.Horizontal(
                rrb.Spatial2DView(origin="world", name="Map & Path"),
                rrb.Vertical(
                    rrb.Tabs(
                        rrb.Spatial2DView(
                            origin="sensors/front/rgb",
                            name="Front Camera",
                        ),
                        rrb.Spatial2DView(
                            origin="planner/navdp/input",
                            name="NavDP Plan",
                        ),
                        active_tab="Front Camera",
                        name="Visuals",
                    ),
                    rrb.TextLogView(
                        origin="events",
                        name="Task Events",
                        columns=rrb.TextLogColumns(
                            timeline_columns=["navigation_time"],
                            text_log_columns=["loglevel", "body"],
                        ),
                        format_options=rrb.TextLogFormat(monospace_body=False),
                    ),
                    row_shares=[3, 2],
                    name="Live Monitor",
                ),
                column_shares=[1.65, 1],
                name="Navigation Workspace",
            ),
            rrb.TimePanel(state="collapsed", timeline="navigation_time"),
            rrb.BlueprintPanel(state="collapsed"),
            rrb.SelectionPanel(state="collapsed"),
            auto_views=False,
            auto_layout=False,
        )

    def publish(self, message, record=True):
        self._enqueue(("message", {"message": deepcopy(message), "record": bool(record)}))

    def publish_image(self, key, content, content_type="image/jpeg", timestamp=None):
        self._enqueue(
            (
                "image",
                {
                    "key": str(key),
                    "content": bytes(content),
                    "content_type": str(content_type),
                    "timestamp": time() if timestamp is None else float(timestamp),
                },
            )
        )

    def update_map(self, metadata):
        payload = deepcopy(metadata)
        payload.setdefault("_rerun_timestamp", time())
        self._enqueue(("map", payload))

    def start_task(self, task_id, goal_text=None, recording_path=None):
        self._enqueue(
            (
                "task_start",
                {
                    "task_id": str(task_id),
                    "goal_text": goal_text,
                    "recording_path": str(recording_path) if recording_path else None,
                    "timestamp": time(),
                },
            )
        )

    def finish_task(self, task_id, status):
        self._enqueue(
            (
                "task_finish",
                {"task_id": str(task_id), "status": status, "timestamp": time()},
            )
        )

    def shutdown(self, timeout=2.0):
        if not self.enabled and self._worker is None:
            return True
        self._enqueue(("shutdown", None), force=True)
        if self._worker is not None:
            self._worker.join(timeout=timeout)
        return self._worker is None or not self._worker.is_alive()

    def _enqueue(self, item, force=False):
        if not self.enabled and not force:
            return False
        try:
            self._queue.put_nowait(item)
            return True
        except queue.Full:
            try:
                self._queue.get_nowait()
                self._queue.task_done()
            except queue.Empty:
                pass
            try:
                self._queue.put_nowait(item)
                return True
            except queue.Full:
                return False

    def _run(self):
        while True:
            try:
                kind, payload = self._queue.get(timeout=0.2)
            except queue.Empty:
                continue
            try:
                if kind == "shutdown":
                    break
                if kind == "map":
                    self._map_metadata = payload
                    self._log_map(self._live, payload)
                    self._log_map(self._task, payload)
                elif kind == "task_start":
                    self._start_task_stream(payload)
                elif kind == "task_finish":
                    self._finish_task_stream(payload)
                elif kind == "image":
                    self._log_image(self._live, payload)
                    self._log_image(self._task, payload)
                elif kind == "message":
                    self._log_message(self._live, payload["message"])
                    if payload["record"]:
                        self._log_message(self._task, payload["message"])
            except Exception:
                logger.exception("Failed to publish visualization data to Rerun")
            finally:
                self._queue.task_done()

        self._disconnect_stream(self._task)
        self._task = None
        self._disconnect_stream(self._live)
        self._live = None
        self.enabled = False

    def _start_task_stream(self, payload):
        self._disconnect_stream(self._task)
        self._task = None
        self._active_task_id = payload["task_id"]
        self._active_task_path = None
        if not self._save_rrd:
            return

        recording_path = payload.get("recording_path")
        if recording_path is None:
            task_dir = self._history_dir / f"{int(payload['timestamp'] * 1000)}-{payload['task_id']}"
            recording_path = task_dir / "recording.rrd"
        recording_path = Path(recording_path)
        recording_path.parent.mkdir(parents=True, exist_ok=True)

        self._task = self._rr.RecordingStream(
            "woosh_robot_navigation",
            recording_id=uuid.uuid4(),
        )
        self._task.save(recording_path)
        self._task.send_blueprint(
            self._build_default_blueprint(),
            make_active=True,
            make_default=True,
        )
        self._active_task_path = recording_path
        self._log_map(self._task, self._map_metadata)
        self._log_text(
            self._task,
            "events/navigation",
            f"task_started: {payload.get('goal_text') or payload['task_id']}",
            "INFO",
            payload["timestamp"],
        )

    def _finish_task_stream(self, payload):
        if payload["task_id"] != self._active_task_id:
            return
        self._log_text(
            self._task,
            "events/navigation",
            f"task_finished: {payload.get('status')}",
            "INFO",
            payload["timestamp"],
        )
        if self._task is not None:
            try:
                self._task.flush(timeout_sec=2.0)
            except Exception:
                logger.exception("Failed to flush Rerun task recording")
        self._disconnect_stream(self._task)
        self._task = None
        self._active_task_id = None
        self._active_task_path = None

    def _log_map(self, stream, metadata):
        if stream is None or not metadata:
            return
        self._set_time(stream, metadata.get("_rerun_timestamp", time()))
        stream.log("world", self._rr.Clear(recursive=True))
        stream.log(
            "world/dynamic",
            self._rr.AnnotationContext(
                [
                    (0, "Free", (0, 0, 0, 0)),
                    (1, "Inflated", (214, 172, 70, 155)),
                    (2, "Occupied", (245, 82, 65, 225)),
                ]
            ),
            static=True,
        )
        self._log_map_layers(stream, metadata)
        self._map_refresh_at[id(stream)] = time()

    def _log_map_layers(self, stream, metadata):
        image_content = metadata.get("_rerun_image_bytes")
        image_media_type = metadata.get("_rerun_image_media_type", "image/png")
        image_path = metadata.get("image_path")
        if image_content:
            stream.log(
                "world/map",
                self._rr.EncodedImage(
                    contents=image_content,
                    media_type=image_media_type,
                    draw_order=-100.0,
                ),
                static=False,
            )
        elif image_path and Path(image_path).is_file():
            stream.log(
                "world/map",
                self._rr.EncodedImage(path=image_path, draw_order=-100.0),
                static=False,
            )
        polygons = metadata.get("only_global_planner_area") or []
        pixel_polygons = [self._points_to_pixels(polygon, metadata) for polygon in polygons]
        pixel_polygons = [polygon for polygon in pixel_polygons if len(polygon) >= 2]
        if pixel_polygons:
            stream.log(
                "world/planner_allowed_area",
                self._rr.LineStrips2D(
                    pixel_polygons,
                    colors=[55, 196, 225, 155],
                    radii=self._rr.Radius.ui_points(1.5),
                    draw_order=2.0,
                ),
                static=False,
            )

    def _log_image(self, stream, payload):
        if stream is None:
            return
        self._set_time(stream, payload["timestamp"])
        entity = {
            "rgb_latest": "sensors/front/rgb",
            "rgb_navdp": "planner/navdp/input",
        }.get(payload["key"], f"sensors/{payload['key']}")
        stream.log(
            entity,
            self._rr.EncodedImage(
                contents=payload["content"],
                media_type=payload["content_type"],
            ),
        )
        try:
            from PIL import Image

            with Image.open(BytesIO(payload["content"])) as image:
                self._image_sizes[payload["key"]] = image.size
        except Exception:
            pass
        if payload["key"] == "rgb_navdp" and self._latest_planner is not None:
            self._log_navdp_projection(stream, self._latest_planner)

    def _log_message(self, stream, message):
        if stream is None:
            return
        timestamp = float(message.get("timestamp", time()))
        self._set_time(stream, timestamp)
        message_type = message.get("type")
        if message_type == "pose_update":
            self._log_pose(
                stream,
                "world/robot",
                message.get("pose"),
                [42, 203, 225],
                label="Robot 定位",
                marker="robot",
                draw_order=30.0,
            )
            self._log_pose(
                stream,
                "world/vpr_pose",
                message.get("vpr_pose"),
                [167, 139, 250],
                label="VPR 定位",
                marker="vpr",
                draw_order=24.0,
            )
        elif message_type == "goal_update":
            goal = message.get("goal") or {}
            self._log_pose(
                stream,
                "world/goal",
                goal.get("pose"),
                [255, 113, 91],
                label="导航目标",
                marker="goal",
                draw_order=28.0,
            )
        elif message_type == "planner_update":
            self._latest_planner = deepcopy(message)
            self._log_path(stream, "world/planner/global_path", message.get("global_path"), [77, 222, 155])
            self._log_path(stream, "world/planner/local_path", message.get("local_path"), [255, 191, 71])
            self._log_points(stream, "world/planner/waypoints", message.get("waypoints"), [167, 139, 250])
            local_goal = message.get("local_goal")
            self._log_points(stream, "world/planner/local_goal", [local_goal] if local_goal else [], [255, 220, 92])
            self._log_scalar(stream, "metrics/planner/action_count", len(message.get("actions") or []))
            self._log_scalar(stream, "metrics/planner/action_limit", message.get("action_limit"))
            stream.log(
                "planner/action_plan",
                self._rr.TextDocument(
                    json.dumps(
                        {
                            "mode": message.get("mode"),
                            "actions": message.get("actions") or [],
                            "action_limit": message.get("action_limit"),
                        },
                        ensure_ascii=False,
                        indent=2,
                    ),
                    media_type="text/markdown",
                ),
            )
            self._log_navdp_projection(stream, message)
        elif message_type == "dynamic_map_update":
            self._log_dynamic_map(stream, message)
        elif message_type == "task_status":
            text = "task_status: " + json.dumps(
                {key: message.get(key) for key in ("task_id", "status", "goal_text", "message")},
                ensure_ascii=False,
            )
            self._log_text(stream, "events/task_status", text, "INFO", timestamp, set_time=False)
        elif message_type == "event":
            text = f"{message.get('event_name')}: {json.dumps(message.get('payload') or {}, ensure_ascii=False)}"
            self._log_text(
                stream,
                "events/navigation",
                text,
                str(message.get("level", "info")).upper(),
                timestamp,
                set_time=False,
            )

    def _log_pose(self, stream, entity, pose, color, *, label, marker, draw_order):
        if not pose or not self._map_metadata:
            stream.log(entity, self._rr.Clear(recursive=True))
            return
        point = self._point_to_pixel([pose["x"], pose["y"]], self._map_metadata)
        outline = [12, 12, 12, 245]
        highlight = [244, 249, 252, 245]
        theta = float(pose.get("theta", 0.0))
        hover_label = (
            f"{label}  ·  x {float(pose['x']):.2f} m  ·  "
            f"y {float(pose['y']):.2f} m  ·  朝向 {math.degrees(theta):.1f}°"
        )
        forward = [math.cos(theta), -math.sin(theta)]

        if marker == "vpr":
            radii = [10.5, 8.4, 3.2]
            colors = [outline, color, highlight]
        elif marker == "robot":
            # The robot is intentionally a little larger than VPR so the
            # primary pose remains dominant when both estimates overlap.
            radii = [13.5, 11.0, 3.8]
            colors = [outline, color, highlight]
        else:
            radii = [12.0, 9.5, 3.4]
            colors = [outline, color, highlight]

        positions = [point for _ in radii]
        # Keep labels hidden in the map and attach the readable hover text to
        # only one layer. Repeating it for every concentric point makes Rerun
        # stack identical labels when a marker is hovered.
        labels = ["" for _ in radii]
        labels[-1] = hover_label
        stream.log(
            entity,
            self._rr.Points2D(
                positions,
                colors=colors,
                radii=[self._rr.Radius.ui_points(radius) for radius in radii],
                labels=labels,
                show_labels=False,
                draw_order=[draw_order + index for index in range(len(radii))],
            ),
        )

        if marker != "vpr":
            start_length = 7.0 if marker == "robot" else 6.0
            direction_length = 20.0 if marker == "robot" else 18.0
            start = [
                point[0] + forward[0] * start_length,
                point[1] + forward[1] * start_length,
            ]
            tip = [
                point[0] + forward[0] * direction_length,
                point[1] + forward[1] * direction_length,
            ]
            stream.log(
                entity,
                self._rr.LineStrips2D(
                    [[start, tip]],
                    colors=color,
                    radii=self._rr.Radius.ui_points(2.4),
                    show_labels=False,
                    draw_order=draw_order + len(radii) + 1.0,
                ),
            )

        stream.log(
            entity,
            self._rr.AnyValues(
                localization_source=label,
                position_x_m=float(pose["x"]),
                position_y_m=float(pose["y"]),
                heading_rad=theta,
                heading_deg=math.degrees(theta),
            ),
        )

    def _log_path(self, stream, entity, points, color):
        pixel_points = self._points_to_pixels(points or [], self._map_metadata)
        if len(pixel_points) < 2:
            stream.log(entity, self._rr.Clear(recursive=True))
            return
        stream.log(
            entity,
            self._rr.LineStrips2D(
                [pixel_points],
                colors=color,
                radii=self._rr.Radius.ui_points(2.5),
                draw_order=6.0,
            ),
        )

    def _log_points(self, stream, entity, points, color):
        pixel_points = self._points_to_pixels(points or [], self._map_metadata)
        if not pixel_points:
            stream.log(entity, self._rr.Clear(recursive=True))
            return
        stream.log(
            entity,
            self._rr.Points2D(
                pixel_points,
                colors=color,
                radii=self._rr.Radius.ui_points(4.5),
                draw_order=8.0,
            ),
        )

    def _log_dynamic_map(self, stream, message):
        now = time()
        if (
            self._map_metadata
            and now - self._map_refresh_at.get(id(stream), float("-inf")) >= 10.0
        ):
            self._log_map_layers(stream, self._map_metadata)
            self._map_refresh_at[id(stream)] = now
        entity = "world/dynamic/occupancy"
        if message.get("clear"):
            stream.log(entity, self._rr.Clear(recursive=True))
            return
        width = int(message.get("width") or 0)
        height = int(message.get("height") or 0)
        if width <= 0 or height <= 0:
            return
        import numpy as np

        mask = np.zeros(width * height, dtype=np.uint8)
        for start, length, *_ in message.get("inflated_runs") or []:
            start = max(0, int(start))
            mask[start : min(mask.size, start + max(0, int(length)))] = 1
        for start, length, *_ in message.get("occupied_runs") or []:
            start = max(0, int(start))
            mask[start : min(mask.size, start + max(0, int(length)))] = 2
        stream.log(
            entity,
            self._rr.SegmentationImage(mask.reshape((height, width)), opacity=0.55, draw_order=1.0),
        )

    def _log_navdp_projection(self, stream, message):
        entity = "planner/navdp/input/trajectory"
        actions = message.get("preview_actions") or []
        intrinsics = message.get("camera_intrinsics")
        source_size = message.get("camera_image_size")
        display_size = self._image_sizes.get("rgb_navdp")
        if not actions or not intrinsics or not source_size or len(source_size) < 2:
            stream.log(entity, self._rr.Clear(recursive=True))
            return
        source_width, source_height = float(source_size[0]), float(source_size[1])
        if source_width <= 0 or source_height <= 0:
            return
        display_width, display_height = display_size or (source_width, source_height)
        scale_x = float(display_width) / source_width
        scale_y = float(display_height) / source_height
        try:
            fx = float(intrinsics[0][0]) * scale_x
            fy = float(intrinsics[1][1]) * scale_y
            cx = float(intrinsics[0][2]) * scale_x
            cy = float(intrinsics[1][2]) * scale_y
        except (IndexError, TypeError, ValueError):
            return

        local_x = 0.0
        local_y = 0.0
        theta = 0.0
        projected = []
        for action in actions:
            if len(action) < 2:
                continue
            theta = (theta + float(action[0]) + math.pi) % (2 * math.pi) - math.pi
            distance = float(action[1])
            local_x += distance * math.cos(theta)
            local_y += distance * math.sin(theta)
            if local_x <= 1e-4:
                continue
            x = fx * -local_y / local_x + cx
            y = float(display_height) - 1.0 + fy * 0.2 / local_x - cy
            if math.isfinite(x) and math.isfinite(y):
                projected.append([x, y])

        if len(projected) < 2:
            stream.log(entity, self._rr.Clear(recursive=True))
            return
        stream.log(
            entity,
            self._rr.LineStrips2D(
                [projected],
                colors=[255, 80, 120],
                radii=3.0,
                labels=["NavDP preview"],
                draw_order=20.0,
            ),
        )
        stream.log(
            f"{entity}/end",
            self._rr.Points2D([projected[-1]], colors=[255, 230, 80], radii=6.0, draw_order=21.0),
        )

    def _log_scalar(self, stream, entity, value):
        if value is not None:
            stream.log(entity, self._rr.Scalars(float(value)))

    def _log_text(self, stream, entity, text, level, timestamp, set_time=True):
        if stream is None:
            return
        if set_time:
            self._set_time(stream, timestamp)
        stream.log(entity, self._rr.TextLog(text, level=level))

    @staticmethod
    def _set_time(stream, timestamp):
        stream.set_time("navigation_time", timestamp=float(timestamp))

    @staticmethod
    def _point_to_pixel(point, metadata):
        origin = metadata.get("origin") or [0.0, 0.0]
        resolution = float(metadata.get("resolution") or 1.0)
        height = float(metadata.get("height") or 0.0)
        return [
            (float(point[0]) - float(origin[0])) / resolution,
            height - (float(point[1]) - float(origin[1])) / resolution,
        ]

    @classmethod
    def _points_to_pixels(cls, points, metadata):
        if not metadata:
            return []
        return [cls._point_to_pixel(point, metadata) for point in points if point is not None and len(point) >= 2]

    @staticmethod
    def _disconnect_stream(stream):
        if stream is None:
            return
        try:
            stream.disconnect()
        except Exception:
            logger.exception("Failed to disconnect Rerun recording stream")


class RerunSink:
    """Non-blocking client for the isolated Rerun 0.36 worker process."""

    VERSION = _RerunRuntime.VERSION

    def __init__(
        self,
        *,
        enabled=True,
        grpc_port=9876,
        server_memory_limit="256MiB",
        cors_allow_origin=None,
        save_rrd=True,
        history_dir="./log/rerun_history",
        queue_size=256,
        bridge_command=None,
    ):
        self.enabled = False
        self.live_uri = None
        self.error = None
        self._active_task_id = None
        self._queue = queue.Queue(maxsize=max(8, int(queue_size)))
        self._process = None
        self._writer = None
        self._lock = Lock()
        if not enabled:
            return

        repo_root = Path(__file__).resolve().parents[2]
        project_dir = repo_root / "rerun_bridge"
        command = list(bridge_command or [])
        if not command:
            command = [
                "uv",
                "run",
                "--project",
                str(project_dir),
                "--locked",
                "python",
            ]
        worker_config = {
            "enabled": True,
            "grpc_port": int(grpc_port),
            "server_memory_limit": str(server_memory_limit),
            "cors_allow_origin": list(cors_allow_origin or []),
            "save_rrd": bool(save_rrd),
            "history_dir": str(history_dir),
            "queue_size": max(8, int(queue_size)),
        }
        try:
            self._process = subprocess.Popen(
                [*command, str(Path(__file__).resolve()), "--worker", json.dumps(worker_config)],
                cwd=str(repo_root),
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=None,
                env={**os.environ, "PYTHONUNBUFFERED": "1"},
            )
            self.live_uri = f"rerun+http://127.0.0.1:{int(grpc_port)}/proxy"
            self.enabled = True
            self._writer = Thread(target=self._write_commands, name="rerun-ipc", daemon=True)
            self._writer.start()
        except Exception as exc:
            self.error = str(exc)
            logger.exception("Unable to launch isolated Rerun worker; continuing without it")

    def status(self):
        with self._lock:
            process = self._process
            if process is not None and process.poll() is not None and self.enabled:
                self.enabled = False
                self.error = f"Rerun worker exited with code {process.returncode}"
            return {
                "enabled": self.enabled,
                "version": self.VERSION,
                "live_uri": self.live_uri if self.enabled else None,
                "error": self.error,
                "active_task_id": self._active_task_id,
                "worker_pid": process.pid if process is not None and process.poll() is None else None,
            }

    def publish(self, message, record=True):
        return self._enqueue(("publish", (deepcopy(message), bool(record))))

    def publish_image(self, key, content, content_type="image/jpeg", timestamp=None):
        return self._enqueue(
            (
                "publish_image",
                (
                    str(key),
                    bytes(content),
                    str(content_type),
                    time() if timestamp is None else float(timestamp),
                ),
            )
        )

    def update_map(self, metadata):
        return self._enqueue(("update_map", (deepcopy(metadata),)))

    def start_task(self, task_id, goal_text=None, recording_path=None):
        self._active_task_id = str(task_id)
        return self._enqueue(
            (
                "start_task",
                (str(task_id), goal_text, str(recording_path) if recording_path else None),
            )
        )

    def finish_task(self, task_id, status):
        self._active_task_id = None
        return self._enqueue(("finish_task", (str(task_id), status)))

    def shutdown(self, timeout=3.0):
        process = self._process
        if process is None:
            return True
        self._enqueue(("shutdown", ()), force=True)
        if self._writer is not None:
            self._writer.join(timeout=max(0.0, float(timeout)))
        try:
            process.wait(timeout=max(0.0, float(timeout)))
        except subprocess.TimeoutExpired:
            process.terminate()
            try:
                process.wait(timeout=1.0)
            except subprocess.TimeoutExpired:
                process.kill()
        self.enabled = False
        return process.poll() is not None

    def _enqueue(self, item, force=False):
        if not self.enabled and not force:
            return False
        try:
            self._queue.put_nowait(item)
            return True
        except queue.Full:
            try:
                self._queue.get_nowait()
                self._queue.task_done()
            except queue.Empty:
                pass
            try:
                self._queue.put_nowait(item)
                return True
            except queue.Full:
                return False

    def _write_commands(self):
        process = self._process
        if process is None or process.stdin is None:
            return
        try:
            while True:
                command = self._queue.get()
                try:
                    payload = pickle.dumps(command, protocol=pickle.HIGHEST_PROTOCOL)
                    process.stdin.write(struct.pack("!I", len(payload)))
                    process.stdin.write(payload)
                    process.stdin.flush()
                    if command[0] == "shutdown":
                        return
                finally:
                    self._queue.task_done()
        except (BrokenPipeError, OSError) as exc:
            with self._lock:
                self.enabled = False
                self.error = f"Rerun worker connection failed: {exc}"
        finally:
            try:
                process.stdin.close()
            except OSError:
                pass


def _read_exact(stream, size):
    chunks = []
    remaining = size
    while remaining:
        chunk = stream.read(remaining)
        if not chunk:
            return None
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def _run_worker(config):
    runtime = _RerunRuntime(**config)
    if not runtime.enabled:
        return 2
    stream = sys.stdin.buffer
    while True:
        header = _read_exact(stream, 4)
        if header is None:
            break
        payload = _read_exact(stream, struct.unpack("!I", header)[0])
        if payload is None:
            break
        command, args = pickle.loads(payload)
        if command == "shutdown":
            break
        getattr(runtime, command)(*args)
    runtime.shutdown(timeout=5.0)
    return 0


if __name__ == "__main__" and len(sys.argv) >= 3 and sys.argv[1] == "--worker":
    raise SystemExit(_run_worker(json.loads(sys.argv[2])))
