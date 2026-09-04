from db import get_user


class AuthService:
    def login(self, username: str) -> dict[str, str]:
        user = get_user(username)
        return {"token": f"token-{user['name']}"}

    def logout(self, token: str) -> bool:
        return bool(token)
