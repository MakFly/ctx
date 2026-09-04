from fastapi import FastAPI

from auth import AuthService

app = FastAPI()
auth = AuthService()


@app.post("/login")
def login_route(username: str) -> dict[str, str]:
    return auth.login(username)
