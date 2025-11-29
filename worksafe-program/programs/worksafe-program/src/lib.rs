use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{self, Mint, Token, TokenAccount, Transfer},
};

declare_id!("CrzDGZhXyyTwtA8JfeZzxB2YyMXfvC3z5dVPP57f2EvR");

const ESCROW_SEED: &[u8] = b"escrow";
const FEE_BPS: u16 = 200; // 2%
const BPS_DENOMINATOR: u16 = 10_000;

#[program]
pub mod worksafe_program {
    use super::*;

    pub fn initialize_escrow(
        ctx: Context<InitializeEscrow>,
        deal_id: [u8; 32],
        amount: u64,
    ) -> Result<()> {
        require!(amount > 0, WorkSafeError::InvalidAmount);

        let escrow = &mut ctx.accounts.escrow;
        escrow.client = ctx.accounts.client.key();
        escrow.freelancer = ctx.accounts.freelancer.key();
        escrow.arbiter = ctx.accounts.arbiter.key();
        escrow.fee_recipient = ctx.accounts.fee_recipient.key();
        escrow.token_mint = ctx.accounts.token_mint.key();
        escrow.amount = amount;
        escrow.deal_id = deal_id;
        escrow.state = EscrowState::Init;
        escrow.bump = ctx.bumps.escrow;

        Ok(())
    }

    pub fn fund_escrow(ctx: Context<FundEscrow>) -> Result<()> {
        let escrow = &mut ctx.accounts.escrow;
        require!(
            escrow.state == EscrowState::Init,
            WorkSafeError::InvalidState
        );
        require_keys_eq!(
            escrow.client,
            ctx.accounts.client.key(),
            WorkSafeError::Unauthorized
        );
        require_keys_eq!(
            escrow.token_mint,
            ctx.accounts.token_mint.key(),
            WorkSafeError::InvalidMint
        );

        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.client_ata.to_account_info(),
                    to: ctx.accounts.escrow_ata.to_account_info(),
                    authority: ctx.accounts.client.to_account_info(),
                },
            ),
            escrow.amount,
        )?;

        escrow.state = EscrowState::Funded;
        Ok(())
    }

    pub fn release_to_freelancer(ctx: Context<ReleaseToFreelancer>) -> Result<()> {
        let escrow = &mut ctx.accounts.escrow;
        require!(
            escrow.state == EscrowState::Funded || escrow.state == EscrowState::Disputed,
            WorkSafeError::InvalidState
        );

        let authority_key = ctx.accounts.authority.key();
        if escrow.state == EscrowState::Disputed {
            require!(authority_key == escrow.arbiter, WorkSafeError::Unauthorized);
        } else {
            require!(
                authority_key == escrow.client || authority_key == escrow.arbiter,
                WorkSafeError::Unauthorized
            );
        }
        require_keys_eq!(
            escrow.freelancer,
            ctx.accounts.freelancer.key(),
            WorkSafeError::Unauthorized
        );
        require_keys_eq!(
            escrow.fee_recipient,
            ctx.accounts.fee_recipient.key(),
            WorkSafeError::Unauthorized
        );

        let fee = compute_fee(escrow.amount)?;
        let payout = escrow
            .amount
            .checked_sub(fee)
            .ok_or(WorkSafeError::MathError)?;

        let bump = [escrow.bump];
        let signer_seeds: &[&[&[u8]]] = &[&[ESCROW_SEED, escrow.deal_id.as_ref(), &bump]];

        if fee > 0 {
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.escrow_ata.to_account_info(),
                        to: ctx.accounts.fee_recipient_ata.to_account_info(),
                        authority: escrow.to_account_info(),
                    },
                    signer_seeds,
                ),
                fee,
            )?;
        }

        if payout > 0 {
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.escrow_ata.to_account_info(),
                        to: ctx.accounts.freelancer_ata.to_account_info(),
                        authority: escrow.to_account_info(),
                    },
                    signer_seeds,
                ),
                payout,
            )?;
        }

        escrow.state = EscrowState::Released;
        Ok(())
    }

    pub fn refund_client(ctx: Context<RefundClient>) -> Result<()> {
        let escrow = &mut ctx.accounts.escrow;
        require!(
            escrow.state == EscrowState::Funded,
            WorkSafeError::InvalidState
        );
        require_keys_eq!(
            escrow.client,
            ctx.accounts.client.key(),
            WorkSafeError::Unauthorized
        );

        let bump = [escrow.bump];
        let signer_seeds: &[&[&[u8]]] = &[&[ESCROW_SEED, escrow.deal_id.as_ref(), &bump]];

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.escrow_ata.to_account_info(),
                    to: ctx.accounts.client_ata.to_account_info(),
                    authority: escrow.to_account_info(),
                },
                signer_seeds,
            ),
            escrow.amount,
        )?;

        escrow.state = EscrowState::Refunded;
        Ok(())
    }

    pub fn raise_dispute(ctx: Context<RaiseDispute>) -> Result<()> {
        let escrow = &mut ctx.accounts.escrow;
        require!(
            escrow.state == EscrowState::Funded,
            WorkSafeError::InvalidState
        );

        let caller = ctx.accounts.authority.key();
        require!(
            caller == escrow.client || caller == escrow.freelancer,
            WorkSafeError::Unauthorized
        );

        escrow.state = EscrowState::Disputed;
        Ok(())
    }

    pub fn admin_resolve(ctx: Context<AdminResolve>, outcome: ResolutionOutcome) -> Result<()> {
        let escrow = &mut ctx.accounts.escrow;
        require!(
            escrow.state == EscrowState::Disputed,
            WorkSafeError::InvalidState
        );
        require_keys_eq!(
            escrow.arbiter,
            ctx.accounts.arbiter.key(),
            WorkSafeError::Unauthorized
        );
        require_keys_eq!(
            escrow.freelancer,
            ctx.accounts.freelancer.key(),
            WorkSafeError::Unauthorized
        );
        require_keys_eq!(
            escrow.fee_recipient,
            ctx.accounts.fee_recipient.key(),
            WorkSafeError::Unauthorized
        );

        let bump = [escrow.bump];
        let signer_seeds: &[&[&[u8]]] = &[&[ESCROW_SEED, escrow.deal_id.as_ref(), &bump]];

        match outcome {
            ResolutionOutcome::Release => {
                let fee = compute_fee(escrow.amount)?;
                let payout = escrow
                    .amount
                    .checked_sub(fee)
                    .ok_or(WorkSafeError::MathError)?;

                if fee > 0 {
                    token::transfer(
                        CpiContext::new_with_signer(
                            ctx.accounts.token_program.to_account_info(),
                            Transfer {
                                from: ctx.accounts.escrow_ata.to_account_info(),
                                to: ctx.accounts.fee_recipient_ata.to_account_info(),
                                authority: escrow.to_account_info(),
                            },
                            signer_seeds,
                        ),
                        fee,
                    )?;
                }

                if payout > 0 {
                    token::transfer(
                        CpiContext::new_with_signer(
                            ctx.accounts.token_program.to_account_info(),
                            Transfer {
                                from: ctx.accounts.escrow_ata.to_account_info(),
                                to: ctx.accounts.freelancer_ata.to_account_info(),
                                authority: escrow.to_account_info(),
                            },
                            signer_seeds,
                        ),
                        payout,
                    )?;
                }
                escrow.state = EscrowState::Released;
            }
            ResolutionOutcome::Refund => {
                token::transfer(
                    CpiContext::new_with_signer(
                        ctx.accounts.token_program.to_account_info(),
                        Transfer {
                            from: ctx.accounts.escrow_ata.to_account_info(),
                            to: ctx.accounts.client_ata.to_account_info(),
                            authority: escrow.to_account_info(),
                        },
                        signer_seeds,
                    ),
                    escrow.amount,
                )?;
                escrow.state = EscrowState::Refunded;
            }
            ResolutionOutcome::Split => {
                let half = escrow
                    .amount
                    .checked_div(2)
                    .ok_or(WorkSafeError::MathError)?;
                let remainder = escrow
                    .amount
                    .checked_sub(half)
                    .ok_or(WorkSafeError::MathError)?;

                if half > 0 {
                    token::transfer(
                        CpiContext::new_with_signer(
                            ctx.accounts.token_program.to_account_info(),
                            Transfer {
                                from: ctx.accounts.escrow_ata.to_account_info(),
                                to: ctx.accounts.freelancer_ata.to_account_info(),
                                authority: escrow.to_account_info(),
                            },
                            signer_seeds,
                        ),
                        half,
                    )?;
                }

                if remainder > 0 {
                    token::transfer(
                        CpiContext::new_with_signer(
                            ctx.accounts.token_program.to_account_info(),
                            Transfer {
                                from: ctx.accounts.escrow_ata.to_account_info(),
                                to: ctx.accounts.client_ata.to_account_info(),
                                authority: escrow.to_account_info(),
                            },
                            signer_seeds,
                        ),
                        remainder,
                    )?;
                }

                escrow.state = EscrowState::Released;
            }
        }

        Ok(())
    }
}

#[derive(Accounts)]
#[instruction(deal_id: [u8; 32])]
pub struct InitializeEscrow<'info> {
    #[account(mut)]
    pub client: Signer<'info>,
    /// CHECK: freelancer wallet, validated via key comparison only
    pub freelancer: UncheckedAccount<'info>,
    /// CHECK: arbiter/admin wallet, validated via key comparison only
    pub arbiter: UncheckedAccount<'info>,
    /// CHECK: fee recipient wallet, validated via key comparison only
    pub fee_recipient: UncheckedAccount<'info>,
    pub token_mint: Account<'info, Mint>,
    #[account(
        init,
        payer = client,
        space = EscrowAccount::LEN,
        seeds = [ESCROW_SEED, deal_id.as_ref()],
        bump
    )]
    pub escrow: Account<'info, EscrowAccount>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct FundEscrow<'info> {
    #[account(mut)]
    pub client: Signer<'info>,
    #[account(
        mut,
        seeds = [ESCROW_SEED, escrow.deal_id.as_ref()],
        bump = escrow.bump,
        constraint = escrow.state == EscrowState::Init
    )]
    pub escrow: Account<'info, EscrowAccount>,
    #[account(constraint = token_mint.key() == escrow.token_mint)]
    pub token_mint: Account<'info, Mint>,
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = client
    )]
    pub client_ata: Account<'info, TokenAccount>,
    #[account(
        init_if_needed,
        payer = client,
        associated_token::mint = token_mint,
        associated_token::authority = escrow
    )]
    pub escrow_ata: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ReleaseToFreelancer<'info> {
    #[account(mut)]
    pub authority: Signer<'info>, // client or arbiter
    #[account(
        mut,
        seeds = [ESCROW_SEED, escrow.deal_id.as_ref()],
        bump = escrow.bump,
        constraint = escrow.state == EscrowState::Funded || escrow.state == EscrowState::Disputed
    )]
    pub escrow: Account<'info, EscrowAccount>,
    #[account(constraint = token_mint.key() == escrow.token_mint)]
    pub token_mint: Account<'info, Mint>,
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = escrow
    )]
    pub escrow_ata: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = freelancer
    )]
    pub freelancer_ata: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = fee_recipient
    )]
    pub fee_recipient_ata: Account<'info, TokenAccount>,
    /// CHECK: freelancer wallet, validated via address constraint
    #[account(address = escrow.freelancer)]
    pub freelancer: UncheckedAccount<'info>,
    /// CHECK: fee recipient wallet, validated via address constraint
    #[account(address = escrow.fee_recipient)]
    pub fee_recipient: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct RefundClient<'info> {
    #[account(mut)]
    pub client: Signer<'info>,
    #[account(
        mut,
        seeds = [ESCROW_SEED, escrow.deal_id.as_ref()],
        bump = escrow.bump,
        constraint = escrow.state == EscrowState::Funded
    )]
    pub escrow: Account<'info, EscrowAccount>,
    #[account(constraint = token_mint.key() == escrow.token_mint)]
    pub token_mint: Account<'info, Mint>,
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = escrow
    )]
    pub escrow_ata: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = client
    )]
    pub client_ata: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct RaiseDispute<'info> {
    #[account(mut)]
    pub authority: Signer<'info>, // client or freelancer
    #[account(
        mut,
        seeds = [ESCROW_SEED, escrow.deal_id.as_ref()],
        bump = escrow.bump,
        constraint = escrow.state == EscrowState::Funded
    )]
    pub escrow: Account<'info, EscrowAccount>,
}

#[derive(Accounts)]
pub struct AdminResolve<'info> {
    #[account(mut)]
    pub arbiter: Signer<'info>,
    #[account(
        mut,
        seeds = [ESCROW_SEED, escrow.deal_id.as_ref()],
        bump = escrow.bump,
        constraint = escrow.state == EscrowState::Disputed
    )]
    pub escrow: Account<'info, EscrowAccount>,
    #[account(constraint = token_mint.key() == escrow.token_mint)]
    pub token_mint: Account<'info, Mint>,
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = escrow
    )]
    pub escrow_ata: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = freelancer
    )]
    pub freelancer_ata: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = fee_recipient
    )]
    pub fee_recipient_ata: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = token_mint,
        associated_token::authority = client
    )]
    pub client_ata: Account<'info, TokenAccount>,
    /// CHECK: client wallet, validated via address constraint
    #[account(address = escrow.client)]
    pub client: UncheckedAccount<'info>,
    /// CHECK: freelancer wallet, validated via address constraint
    #[account(address = escrow.freelancer)]
    pub freelancer: UncheckedAccount<'info>,
    /// CHECK: fee recipient wallet, validated via address constraint
    #[account(address = escrow.fee_recipient)]
    pub fee_recipient: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

#[account]
pub struct EscrowAccount {
    pub client: Pubkey,
    pub freelancer: Pubkey,
    pub arbiter: Pubkey,
    pub fee_recipient: Pubkey,
    pub token_mint: Pubkey,
    pub amount: u64,
    pub state: EscrowState,
    pub bump: u8,
    pub deal_id: [u8; 32],
}

impl EscrowAccount {
    pub const LEN: usize = 8 + (32 * 5) + 8 + 1 + 1 + 32;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EscrowState {
    Init = 0,
    Funded = 1,
    Released = 2,
    Refunded = 3,
    Disputed = 4,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResolutionOutcome {
    Release = 0,
    Refund = 1,
    Split = 2,
}

fn compute_fee(amount: u64) -> Result<u64> {
    Ok(amount
        .checked_mul(FEE_BPS as u64)
        .ok_or(WorkSafeError::MathError)?
        / (BPS_DENOMINATOR as u64))
}

#[error_code]
pub enum WorkSafeError {
    #[msg("Invalid state for this operation.")]
    InvalidState,
    #[msg("Invalid mint provided.")]
    InvalidMint,
    #[msg("The provided amount must be greater than zero.")]
    InvalidAmount,
    #[msg("Unauthorized signer for this operation.")]
    Unauthorized,
    #[msg("Math error during calculation.")]
    MathError,
    #[msg("Missing bump seed.")]
    BumpNotFound,
}
