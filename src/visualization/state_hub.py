from collections import deque
from threading import Lock
from time import time

from .models import (
    SCHEMA_VERSION,
    build_snapshot,
    default_goal_state,
    default_images_state,
    default_planner_state,
    default_robot_state,
    default_task_state,
)

_UNSET = object()


class VisualizationStateHub:
    def __init__(self, max_event_buffer=2000, replay_store=None, save_replay=False, rerun_sink=None):
        self._lock = Lock()
        self._task = default_task_state()
        self._goal = default_goal_state()
        self._robot = default_robot_state()
        self._planner = default_planner_state()
        self._images = default_images_state()
        self._dynamic_map = None
        self._last_recorded_dynamic_map = None
        self._dynamic_map_recording = False
        self._events = deque(maxlen=max_event_buffer)
        self.schema_version = SCHEMA_VERSION
        self._replay_store = replay_store
        self._save_replay = save_replay
        self._rerun_sink = rerun_sink
        self._active_task_id = None
        self._broadcast = None

    def set_broadcast(self, broadcast):
        self._broadcast = broadcast

    def _emit(self, message):
        if self._broadcast is not None:
            self._broadcast(message)

    def _emit_all(self, message, record_rerun=True):
        if self._rerun_sink is not None:
            self._rerun_sink.publish(message, record=record_rerun)
        self._emit(message)

    def _resolve_replay_task_id(self, task_id=None, allow_active_task=False):
        if not self._save_replay or self._replay_store is None:
            return None

        with self._lock:
            active_task_id = self._active_task_id

        if active_task_id is None:
            return None
        if task_id is not None and task_id != active_task_id:
            return None
        if task_id is None and not allow_active_task:
            return None
        return active_task_id

    def _append_replay_message(self, message, task_id=None, allow_active_task=False, replay_message=None):
        replay_task_id = self._resolve_replay_task_id(task_id=task_id, allow_active_task=allow_active_task)
        if replay_task_id is None:
            return
        self._replay_store.append_event(replay_task_id, replay_message or message)

    def _build_replay_image_message(self, key, version, updated_at, replay_url):
        if replay_url is None:
            return None
        return {
            "type": "image_update",
            "schema_version": self.schema_version,
            "timestamp": updated_at,
            "images": {
                key: {
                    "url": replay_url,
                    "version": version,
                    "updated_at": updated_at,
                }
            },
        }

    def _build_message(self, message_type, **payload):
        return {
            "type": message_type,
            "schema_version": self.schema_version,
            "timestamp": time(),
            **payload,
        }

    def update_task_status(
        self,
        task_id=_UNSET,
        status=_UNSET,
        result_status=_UNSET,
        goal_text=_UNSET,
        success_flag=_UNSET,
        message=_UNSET,
        state=_UNSET,
        dry_run=_UNSET,
    ):
        with self._lock:
            if task_id is not _UNSET:
                self._task["task_id"] = task_id
            if status is not _UNSET:
                self._task["status"] = status
            if result_status is not _UNSET:
                self._task["result_status"] = result_status
            if goal_text is not _UNSET:
                self._task["goal_text"] = goal_text
            if success_flag is not _UNSET:
                self._task["success_flag"] = success_flag
            if message is not _UNSET:
                self._task["message"] = message
            if state is not _UNSET:
                self._task["state"] = state
            if dry_run is not _UNSET:
                self._task["dry_run"] = bool(dry_run)
            task_payload = dict(self._task)
        message = self._build_message("task_status", **task_payload)
        self._append_replay_message(message, task_id=message.get("task_id"), allow_active_task=True)
        self._emit_all(message)

    def update_goal(self, x, y, theta, source=None):
        with self._lock:
            self._goal["pose"] = {"x": x, "y": y, "theta": theta}
            self._goal["source"] = source
            goal_payload = dict(self._goal)
        message = self._build_message("goal_update", goal=goal_payload)
        self._append_replay_message(message, allow_active_task=True)
        self._emit_all(message)

    def update_pose(self, pose=None, vpr_pose=None):
        with self._lock:
            if pose is not None:
                self._robot["pose"] = dict(pose)
            if vpr_pose is not None:
                self._robot["vpr_pose"] = dict(vpr_pose)
            robot_payload = dict(self._robot)
        message = self._build_message("pose_update", **robot_payload)
        self._append_replay_message(message, allow_active_task=True)
        self._emit_all(message)

    def update_robot(self, pose=None, vpr_pose=None):
        self.update_pose(pose=pose, vpr_pose=vpr_pose)

    def update_planner(
        self,
        mode=None,
        global_path=None,
        waypoints=None,
        local_path=None,
        local_goal=_UNSET,
        actions=None,
        action_limit=_UNSET,
        preview_actions=None,
        camera_intrinsics=_UNSET,
        camera_image_size=_UNSET,
    ):
        with self._lock:
            if mode is not None:
                self._planner["mode"] = mode
            if global_path is not None:
                self._planner["global_path"] = list(global_path)
            if waypoints is not None:
                self._planner["waypoints"] = list(waypoints)
            if local_path is not None:
                self._planner["local_path"] = list(local_path)
            if local_goal is not _UNSET:
                self._planner["local_goal"] = list(local_goal) if local_goal is not None else None
            if actions is not None:
                self._planner["actions"] = list(actions)
            if action_limit is not _UNSET:
                self._planner["action_limit"] = int(action_limit) if action_limit is not None else None
            if preview_actions is not None:
                self._planner["preview_actions"] = list(preview_actions)
            if camera_intrinsics is not _UNSET:
                self._planner["camera_intrinsics"] = (
                    [list(row) for row in camera_intrinsics]
                    if camera_intrinsics is not None
                    else None
                )
            if camera_image_size is not _UNSET:
                self._planner["camera_image_size"] = (
                    [int(value) for value in camera_image_size]
                    if camera_image_size is not None
                    else None
                )
            planner_payload = dict(self._planner)
        message = self._build_message("planner_update", **planner_payload)
        self._append_replay_message(message, allow_active_task=True)
        self._emit_all(message)

    def persist_image_frame(self, key, version, content, content_type="image/jpeg", updated_at=None):
        if self._rerun_sink is not None:
            self._rerun_sink.publish_image(
                key,
                content,
                content_type=content_type,
                timestamp=updated_at,
            )
        replay_task_id = self._resolve_replay_task_id(allow_active_task=True)
        if replay_task_id is None:
            return None
        return self._replay_store.store_frame(
            replay_task_id,
            key=key,
            version=version,
            content=content,
            content_type=content_type,
            updated_at=updated_at,
        )

    def update_image(self, key, url, version, updated_at=None, replay_url=None):
        with self._lock:
            image_updated_at = updated_at if updated_at is not None else time()
            self._images[key] = {
                "url": url,
                "version": version,
                "updated_at": image_updated_at,
            }
            image_payload = {key: dict(self._images[key])}
        message = self._build_message("image_update", images=image_payload)
        replay_message = self._build_replay_image_message(
            key=key,
            version=version,
            updated_at=image_updated_at,
            replay_url=replay_url,
        )
        if replay_message is not None:
            replay_message["timestamp"] = message["timestamp"]
        self._append_replay_message(message, allow_active_task=True, replay_message=replay_message)
        self._emit_all(message)

    def update_images(self, **kwargs):
        with self._lock:
            for key, value in kwargs.items():
                self._images[key] = value
            images_payload = {
                key: (dict(value) if isinstance(value, dict) else value)
                for key, value in kwargs.items()
            }
        message = self._build_message("image_update", images=images_payload)
        self._append_replay_message(message, allow_active_task=True)
        self._emit_all(message)

    def update_dynamic_map(self, payload, timestamp=None):
        # Callers hand over a freshly built payload and the stored frame is only
        # ever read back through build_snapshot, which deep-copies on its way
        # out. Deep-copying the thousands of short run lists here dominated the
        # mapping thread's publish cost, so keep the copy shallow.
        frame = dict(payload)
        frame["clear"] = bool(frame.get("clear", False))
        stored = None if frame["clear"] else frame
        with self._lock:
            self._dynamic_map = stored
            should_record = self._dynamic_map_recording
            if should_record:
                self._last_recorded_dynamic_map = stored
        message = {
            "type": "dynamic_map_update",
            "schema_version": self.schema_version,
            "timestamp": time() if timestamp is None else float(timestamp),
            **frame,
        }
        if should_record:
            self._append_replay_message(message, allow_active_task=True)
        self._emit_all(message, record_rerun=should_record)
        return message

    def clear_dynamic_map(self, map_revision=None, timestamp=None):
        with self._lock:
            if self._dynamic_map is None:
                return None
        return self.update_dynamic_map(
            {
                "revision": 0,
                "map_revision": int(map_revision or 0),
                "width": 0,
                "height": 0,
                "occupied_runs": [],
                "inflated_runs": [],
                "clear": True,
            },
            timestamp=timestamp,
        )

    def get_dynamic_map_recording(self):
        with self._lock:
            return self._dynamic_map_recording

    def set_dynamic_map_recording(self, enabled):
        with self._lock:
            self._dynamic_map_recording = bool(enabled)
            value = self._dynamic_map_recording
        self._emit_all(self._build_message("dynamic_map_recording_update", enabled=value))
        return value

    def append_event(self, event_name, task_id=None, payload=None, level="info"):
        event = {
            "type": "event",
            "schema_version": self.schema_version,
            "timestamp": time(),
            "event_name": event_name,
            "level": level,
            "task_id": task_id,
            "payload": payload or {},
        }
        with self._lock:
            self._events.append(event)
        self._append_replay_message(event, task_id=task_id)
        self._emit_all(event)
        return event

    def build_snapshot(self):
        with self._lock:
            return build_snapshot(
                task=self._task,
                goal=self._goal,
                robot=self._robot,
                planner=self._planner,
                images=self._images,
                events=list(self._events),
                timestamp=time(),
                dynamic_map=self._dynamic_map,
                dynamic_map_recording=self._dynamic_map_recording,
            )

    def build_operator_status(self):
        """Return only low-frequency state needed by an operator UI.

        High-rate pose, planner, image, and dynamic-map data intentionally stay
        out of this payload because Rerun is their single transport.
        """
        with self._lock:
            task = {
                "task_id": self._task.get("task_id"),
                "status": self._task.get("status", "idle"),
                "goal_text": self._task.get("goal_text"),
                "dry_run": bool(self._task.get("dry_run", False)),
            }
            dynamic_map_recording = self._dynamic_map_recording
        return {
            "schema_version": self.schema_version,
            "timestamp": time(),
            "task": task,
            "navigation_running": task.get("status") in {"processing", "busy"},
            "dynamic_map_recording": bool(dynamic_map_recording),
        }

    def start_task_recording(self, task_id, goal_text):
        with self._lock:
            self._active_task_id = task_id
            self._last_recorded_dynamic_map = None
        task_dir = None
        if self._save_replay and self._replay_store is not None:
            task_dir = self._replay_store.start_task(task_id=task_id, goal_text=goal_text)
        if self._rerun_sink is not None:
            recording_path = task_dir / "recording.rrd" if task_dir is not None else None
            self._rerun_sink.start_task(
                task_id=task_id,
                goal_text=goal_text,
                recording_path=recording_path,
            )
        return task_dir

    def finalize_task_recording(self, task_id, status, async_write=False):
        with self._lock:
            is_active = task_id == self._active_task_id
            snapshot = None
            if is_active:
                snapshot = build_snapshot(
                    task=self._task,
                    goal=self._goal,
                    robot=self._robot,
                    planner=self._planner,
                    images=self._images,
                    events=list(self._events),
                    timestamp=time(),
                    dynamic_map=self._last_recorded_dynamic_map,
                    dynamic_map_recording=self._dynamic_map_recording,
                )
                self._active_task_id = None
        if is_active and self._rerun_sink is not None:
            self._rerun_sink.finish_task(task_id=task_id, status=status)
        if self._save_replay and self._replay_store is not None and is_active:
            if async_write:
                return self._replay_store.finalize_task_async(
                    task_id=task_id,
                    status=status,
                    snapshot=snapshot,
                )
            return self._replay_store.finalize_task(
                task_id=task_id,
                status=status,
                snapshot=snapshot,
            )
        return None

    def get_active_task_id(self):
        with self._lock:
            return self._active_task_id
