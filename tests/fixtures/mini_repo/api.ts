import express from "express";

const router = express.Router();

export const createOrder = async (sku: string): Promise<string> => {
  return Promise.resolve(sku);
};

router.post("/orders", createOrder);
