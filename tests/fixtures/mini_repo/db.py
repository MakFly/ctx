USERS = {"ada": {"name": "ada"}}


def get_user(username: str) -> dict[str, str]:
    return USERS[username]


def save_payment(user_id: int, amount: int) -> bool:
    return user_id > 0 and amount > 0
