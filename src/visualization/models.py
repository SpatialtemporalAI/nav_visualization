from copy import deepcopy


SCHEMA_VERSION = "v1"


def default_task_state():
    return {
        "task_id": None,
        "goal_text": None,
        "status": "idle",
        "result_status": None,
        "success_flag": None,
        "message": None,
        "state": None,
        "dry_run": False,
    }


def default_goal_state():
    return {
        "pose": None,
        "source": None,
    }


def default_robot_state():
    return {
        "pose": None,
        "vpr_pose": None,
    }


def default_planner_state():
    return {
        "mode": None,
        "global_path": [],
        "waypoints": [],
        "local_path": [],
        "local_goal": None,
        "actions": [],
        "action_limit": None,
        "preview_actions": [],
        "camera_intrinsics": None,
        "camera_image_size": None,
    }


def default_images_state():
    return {}


def build_snapshot(
    task,
    goal,
    robot,
    planner,
    images,
    events,
    timestamp,
    dynamic_map=None,
    dynamic_map_recording=False,
):
    return {
        "type": "snapshot",
        "schema_version": SCHEMA_VERSION,
        "timestamp": timestamp,
        "task": deepcopy(task),
        "goal": deepcopy(goal),
        "robot": deepcopy(robot),
        "planner": deepcopy(planner),
        "images": deepcopy(images),
        "events": deepcopy(events),
        "dynamic_map": deepcopy(dynamic_map),
        "dynamic_map_recording": bool(dynamic_map_recording),
    }
